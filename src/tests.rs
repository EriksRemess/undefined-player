use crate::{
    Result, cli::*, clock::*, ffi, media::*, metadata::*, mpris::seconds_to_microseconds,
    presentation::*, subtitles::*, window::*,
};
use std::sync::Mutex;
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

static EXTERNAL_MEDIA_TEST_LOCK: Mutex<()> = Mutex::new(());

fn cli(arguments: &[&str]) -> Result<CliAction> {
    parse_cli(arguments.iter().map(OsString::from))
}

#[test]
fn command_line_supports_standard_information_options() {
    assert_eq!(cli(&["--help"]), Ok(CliAction::Help));
    assert_eq!(cli(&["-h"]), Ok(CliAction::Help));
    assert_eq!(cli(&["--version"]), Ok(CliAction::Version));
    assert_eq!(cli(&["-V"]), Ok(CliAction::Version));
}

#[test]
fn command_line_parses_playback_options_and_paths() {
    assert_eq!(
        cli(&["--perf", "movie.mkv"]),
        Ok(CliAction::Play {
            path: PathBuf::from("movie.mkv"),
            perf_log: true,
        })
    );
    assert_eq!(
        cli(&["--", "--unusual-name.mkv"]),
        Ok(CliAction::Play {
            path: PathBuf::from("--unusual-name.mkv"),
            perf_log: false,
        })
    );
}

#[test]
fn command_line_rejects_missing_files_and_unknown_options() {
    assert_eq!(cli(&[]), Err("no media file was specified".into()));
    assert_eq!(cli(&["--wat"]), Err("unknown option: --wat".into()));
    assert_eq!(
        cli(&["one.mkv", "two.mkv"]),
        Err("only one media file can be played at a time".into())
    );
}

#[test]
fn requested_keys_map_to_requested_actions() {
    assert_eq!(
        action_for_key(ffi::UpKey_UP_KEY_A),
        Some(Action::CycleAudio)
    );
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
fn media_tracks_cycle_and_report_available_metadata() {
    assert_eq!(next_track(0, 3), 1);
    assert_eq!(next_track(2, 3), 0);
    assert_eq!(next_track(0, 0), 0);
    let labeled = TrackLabel {
        codec: "DTS".into(),
        language: Some("spa".into()),
        title: Some("Surround 5.1".into()),
    };
    assert_eq!(
        track_status_text("AUDIO", 2, 5, &labeled).to_bytes(),
        "AUDIO: 3 / 5 - SPA - SURROUND 5.1 - DTS".as_bytes()
    );
    let unlabeled = TrackLabel {
        codec: "AC3".into(),
        language: None,
        title: Some("Commentary".into()),
    };
    assert_eq!(
        track_status_text("AUDIO", 1, 2, &unlabeled).to_bytes(),
        b"AUDIO: 2 / 2"
    );
    assert_eq!(
        subtitle_status_text(false, 1, 3, &labeled).to_bytes(),
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
        info.overlay_text(Some((1, 3, &TrackLabel {
            codec: "DTS".into(),
            language: Some("eng".into()),
            title: Some("Surround 5.1".into()),
        }))).to_bytes(),
        b"CODEC: HEVC MAIN 10\nRESOLUTION: 3840X2160\nBITRATE: 18.8 MBPS\nPIXEL FORMAT: YUV420P10LE\nDECODE: VULKAN HW\nMATRIX: BT2020NC\nPRIMARIES: BT2020\nTRANSFER: SMPTE2084\nRANGE: TV\nHDR: YES (PQ)\nAUDIO TRACK: 2 / 3\nAUDIO CODEC: DTS\nAUDIO LANGUAGE: ENG\nAUDIO TITLE: SURROUND 5.1"
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
fn seek_completion_waits_for_audio_and_video_and_uses_audio_clock() {
    assert_eq!(
        completed_seek_anchor(Some(10.0), None, true, false, false, 9.0),
        None
    );
    assert_eq!(
        completed_seek_anchor(None, Some(10.0), true, false, false, 9.0),
        None
    );
    assert_eq!(
        completed_seek_anchor(Some(10.02), Some(10.0), true, false, false, 9.0),
        Some(10.0)
    );
    assert_eq!(
        completed_seek_anchor(Some(10.02), None, false, false, false, 9.0),
        Some(10.02)
    );
    assert_eq!(
        completed_seek_anchor(None, None, true, true, false, 9.0),
        Some(9.0)
    );
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

#[test]
#[ignore = "requires UP_TEST_MEDIA pointing to a local video file"]
fn external_media_reports_complete_video_info() {
    let _serial = EXTERNAL_MEDIA_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = env::var_os("UP_TEST_MEDIA")
        .map(PathBuf::from)
        .expect("UP_TEST_MEDIA is set");
    let _sdl = unsafe { Sdl::init() }.expect("SDL initializes");
    let mut media = unsafe { Media::open(&path, None, false) }.expect("test media opens");
    unsafe { media.fill_initial_queues() }.expect("initial queues fill");
    let info = unsafe {
        VideoInfo::inspect(
            &media,
            media.video_queue.front().expect("video frame").as_ptr(),
        )
    };
    let details = info.overlay_text(media.audio_tracks.first().map(|track| {
        (
            media.selected_audio_track,
            media.audio_tracks.len(),
            &track.label,
        )
    }));
    let details = details.to_string_lossy();
    eprintln!("{details}");
    assert!(
        !details.contains("UNKNOWN"),
        "decodable media has complete effective video information"
    );
}

#[test]
#[ignore = "requires UP_TEST_MEDIA pointing to a local file with multiple audio tracks"]
fn external_media_switches_every_audio_track() {
    let _serial = EXTERNAL_MEDIA_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = env::var_os("UP_TEST_MEDIA")
        .map(PathBuf::from)
        .expect("UP_TEST_MEDIA is set");
    let _sdl = unsafe { Sdl::init() }.expect("SDL initializes");
    let mut media = unsafe { Media::open(&path, None, false) }.expect("test media opens");
    assert!(
        media.audio_tracks.len() > 1,
        "test media has multiple audio tracks"
    );

    unsafe { media.fill_initial_queues() }.expect("initial queues fill");
    let target = media.video_queue.front().map_or(0.0, |frame| frame.pts) + 5.0;
    for track in 1..media.audio_tracks.len() {
        unsafe { media.select_audio_track(track, target) }.expect("audio track switches");
        for _ in 0..128 {
            unsafe { media.fill_queues() }.expect("switched queues fill");
            if !media.video_queue.is_empty() && !unsafe { media.audio_empty() } {
                break;
            }
        }
        assert_eq!(media.selected_audio_track, track);
        assert!(
            !media.video_queue.is_empty(),
            "video resumes after switching"
        );
        assert!(
            !unsafe { media.audio_empty() },
            "selected audio track produces output"
        );
        let video_pts = media.video_queue.front().unwrap().pts;
        let audio_pts = media.audio.as_ref().unwrap().first_pts.unwrap();
        assert!(
            (audio_pts - video_pts).abs() < 0.25,
            "audio/video remain aligned after switching: audio={audio_pts}, video={video_pts}"
        );
    }
}
