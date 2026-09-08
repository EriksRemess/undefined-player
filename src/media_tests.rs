use crate::{
    audio::*,
    clock::{AUDIO_CLOCK_TOLERANCE, WallClock},
    ffi,
    media::Media,
    metadata::VideoInfo,
    window::{Sdl, sdl_error},
    worker::DecodeWorker,
};
use std::process::Command;
use std::{
    collections::VecDeque,
    env,
    path::PathBuf,
    ptr, thread,
    time::{Duration, Instant},
};

// Each SDL test runs in its own process: SDL initialization and environment
// changes must not race the Rust test runner or the optional desktop tests.
fn child(case: &str) {
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "media_tests::playback_child",
            "--ignored",
            "--nocapture",
        ])
        .env("UP_REGRESSION_CASE", case)
        .env("SDL_VIDEODRIVER", "dummy")
        .env("SDL_AUDIODRIVER", "dummy")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{case}: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn changing_audio_format_is_safe() {
    child("changing-audio");
}
#[test]
fn delayed_audio_switch_makes_progress() {
    child("delayed-audio");
}
#[test]
fn raw_video_timestamps_survive_queue_transfers() {
    child("raw-video");
}
#[test]
fn audio_tail_is_bounded() {
    child("audio-tail");
}
#[test]
fn delayed_video_does_not_block_startup() {
    child("delayed-video");
}

#[test]
fn deferred_video_keeps_software_fallback() {
    child("deferred-fallback");
}

#[test]
fn audio_timestamp_gaps_are_scheduled_and_bounded() {
    child("audio-gaps");
}
#[test]
fn video_clock_recovers_after_audio_underrun() {
    child("audio-underrun");
}
#[test]
fn changing_audio_format_preserves_resampler_tail() {
    child("resampler-tail");
}

fn decode_one_audio_packet(media: &mut Media) {
    let stream = media.audio_tracks[media.selected_audio_track]
        .decoder
        .stream_index;
    for _ in 0..128 {
        assert!(unsafe { ffi::up_av_read_frame(media.format, media.packet) } >= 0);
        let is_audio = unsafe { ffi::up_av_packet_stream_index(media.packet) } == stream;
        let result = unsafe { media.decode_packet() };
        unsafe { ffi::up_av_packet_unref(media.packet) };
        result.unwrap();
        if is_audio {
            return;
        }
    }
    panic!("audio packet not found");
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = env::temp_dir().join(format!("undefined-player-test-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn generate(&self, args: &[&str], name: &str) -> PathBuf {
        let path = self.0.join(name);
        let output = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error"])
            .args(args)
            .arg(&path)
            .output()
            .expect("playback regression tests require the ffmpeg command");
        assert!(
            output.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "subprocess entry point for the automatic playback regression tests"]
fn playback_child() {
    let Ok(case) = env::var("UP_REGRESSION_CASE") else {
        return;
    };
    assert_ne!(unsafe { ffi::up_platform_init() }, 0, "{}", unsafe {
        sdl_error()
    });
    let _sdl = Sdl;
    let fixture = Fixture::new();
    match case.as_str() {
        "audio-gaps" => {
            for extra_gap in [0, 3600] {
                // Shift the last two seconds of a three-second source instead
                // of dropping frames: aselect is broken in FFmpeg 8.0.1.
                let filter = format!("asetpts='PTS+if(gte(T,1),{}/TB,0)'", extra_gap + 2);
                let path = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=64x64:rate=25:duration=5",
                        "-f",
                        "lavfi",
                        "-i",
                        "sine=sample_rate=48000:duration=3",
                        "-af",
                        &filter,
                        "-c:v",
                        "mpeg4",
                        "-c:a",
                        "pcm_s16le",
                    ],
                    &format!("gap-{extra_gap}.mkv"),
                );
                let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
                let mut saw_gap = false;
                for _ in 0..256 {
                    unsafe { media.fill_queues() }.unwrap();
                    media.video_queue.clear();
                    let audio = media.audio.as_mut().unwrap();
                    assert!(unsafe { audio.queued_bytes() } <= AUDIO_QUEUE_MAX_BYTES);
                    assert!(
                        audio.pending.iter().map(|c| c.samples.len()).sum::<usize>() < 48_000,
                        "gaps must not allocate silence buffers"
                    );
                    if audio
                        .first_pts
                        .is_some_and(|pts| pts >= f64::from(extra_gap) + 3.0)
                        && !saw_gap
                    {
                        saw_gap = true;
                        let start = audio.first_pts.unwrap();
                        unsafe { media.sync_audio(start - 0.5, false) }.unwrap();
                        assert!(
                            !media.audio.as_ref().unwrap().resumed,
                            "future segment played early"
                        );
                        unsafe { media.sync_audio(start, false) }.unwrap();
                        assert!(media.audio.as_ref().unwrap().resumed);
                        unsafe { media.sync_audio(start, true) }.unwrap();
                    }
                    unsafe { ffi::up_audio_stream_clear(media.audio.as_ref().unwrap().stream) };
                    if media.eof && media.audio.as_ref().unwrap().pending.is_empty() {
                        break;
                    }
                }
                assert!(media.eof, "gap-{extra_gap}: playback did not reach EOF");
                assert!(saw_gap, "gap-{extra_gap}: no future audio segment observed");
                let audio = media.audio.as_ref().unwrap();
                let end = unsafe { audio.clock() }.unwrap();
                assert!(
                    (end - f64::from(extra_gap) - 5.0).abs() < 0.002,
                    "audio ends at {end}"
                );
                assert_eq!(audio.submitted_frames, 3 * 48_000);
            }
        }
        "audio-underrun" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=25:duration=4",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=48000:duration=4",
                    "-c:v",
                    "mpeg4",
                    "-c:a",
                    "pcm_s16le",
                ],
                "underrun.mkv",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { media.fill_initial_queues() }.unwrap();
            let mut clock = WallClock::new(unsafe { media.audio_clock() }.unwrap());
            unsafe { media.sync_audio(clock.now(), false) }.unwrap();
            let buffered = unsafe { media.audio.as_ref().unwrap().queued_bytes() } as f64
                / (f64::from(AUDIO_RATE) * AUDIO_BYTES_PER_FRAME as f64);
            thread::sleep(Duration::from_secs_f64(buffered + 0.2));
            assert!(unsafe { media.audio_empty() });
            assert!(
                media
                    .audio
                    .as_ref()
                    .and_then(|a| unsafe { a.active_clock() })
                    .is_none()
            );
            assert!(clock.now() - unsafe { media.audio_clock() }.unwrap() > AUDIO_CLOCK_TOLERANCE);
            media.video_queue.clear();
            unsafe { media.fill_queues() }.unwrap();
            unsafe { media.sync_audio(clock.now(), false) }.unwrap();
            let audio = media.audio.as_ref().unwrap();
            clock.synchronize(unsafe { audio.active_clock() }.unwrap());
            assert!((clock.now() - unsafe { audio.clock() }.unwrap()).abs() < 0.02);
            thread::sleep(Duration::from_millis(100));
            assert!(
                (clock.now() - unsafe { audio.clock() }.unwrap()).abs() < AUDIO_CLOCK_TOLERANCE
            );
        }
        "resampler-tail" => {
            let mut paths = Vec::new();
            for (rate, channels) in [(44100, "2"), (48000, "1")] {
                let source = format!("sine=sample_rate={rate}:duration=1");
                paths.push(fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=64x64:rate=25:duration=0.04",
                        "-f",
                        "lavfi",
                        "-i",
                        &source,
                        "-af",
                        "atrim=end_sample=1024",
                        "-ac",
                        channels,
                        "-c:v",
                        "mpeg4",
                        "-c:a",
                        "pcm_s16le",
                    ],
                    &format!("tail-{rate}.mkv"),
                ));
            }
            let mut first = unsafe { Media::open(&paths[0], None, false) }.unwrap();
            decode_one_audio_packet(&mut first);
            assert!(
                first.audio.as_ref().unwrap().submitted_frames < 1115,
                "resampler retains a tail"
            );
            let mut second = unsafe { Media::open(&paths[1], None, false) }.unwrap();
            second.audio = first.audio.take();
            decode_one_audio_packet(&mut second);
            let audio = second.audio.as_mut().unwrap();
            unsafe { audio.drain_converter(None) }.unwrap();
            // Consume the old timestamp segment so the next one can be queued.
            unsafe { ffi::up_audio_stream_clear(audio.stream) };
            unsafe { audio.pump() }.unwrap();
            assert_eq!(
                audio.submitted_frames,
                1115 + 1024,
                "format change lost buffered samples"
            );
            assert!(audio.pending.is_empty());
        }

        "raw-video" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=120:duration=1",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-f",
                    "h264",
                ],
                "raw.h264",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            let mut count = 0;
            while !media.eof {
                unsafe { media.fill_queues() }.unwrap();
                while let Some(frame) = media.video_queue.pop_front() {
                    assert!(
                        (frame.pts - f64::from(count) / 120.0).abs() < 0.0001,
                        "frame {count}: PTS {}",
                        frame.pts
                    );
                    count += 1;
                }
            }
            assert_eq!(count, 120);
        }
        "changing-audio" => {
            let mut bytes = Vec::new();
            for (name, channels, rate) in [
                ("stereo.ts", "2", "48000"),
                ("mono.ts", "1", "44100"),
                ("surround.ts", "6", "32000"),
            ] {
                let path = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=64x64:rate=25:duration=1",
                        "-f",
                        "lavfi",
                        "-i",
                        "sine=sample_rate=48000:duration=1",
                        "-c:v",
                        "mpeg2video",
                        "-c:a",
                        "aac",
                        "-ac",
                        channels,
                        "-ar",
                        rate,
                        "-f",
                        "mpegts",
                    ],
                    name,
                );
                bytes.extend(std::fs::read(path).unwrap());
            }
            let path = fixture.0.join("changing.ts");
            std::fs::write(&path, bytes).unwrap();
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            for _ in 0..256 {
                unsafe { media.fill_queues() }.unwrap();
                media.video_queue.clear();
                unsafe { ffi::up_audio_stream_clear(media.audio.as_ref().unwrap().stream) };
                if media.eof {
                    break;
                }
            }
            assert!(media.eof);
            let samples = media.audio.as_ref().unwrap().submitted_frames;
            assert!(
                (140_000..155_000).contains(&samples),
                "converted frames: {samples}"
            );
        }
        "audio-tail" | "delayed-video" | "deferred-fallback" => {
            let mut args = vec!["-f", "lavfi", "-i", "sine=sample_rate=48000:duration=8"];
            if case != "audio-tail" {
                args.extend(["-itsoffset", "3"]);
            }
            args.extend([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=25:duration=0.2",
            ]);
            if case == "deferred-fallback" {
                args.extend([
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=880:sample_rate=48000:duration=8",
                ]);
            }
            args.extend([
                "-map",
                "1:v",
                "-map",
                "0:a",
                "-c:v",
                "mpeg4",
                "-c:a",
                "pcm_s16le",
            ]);
            if case == "deferred-fallback" {
                args.extend(["-map", "2:a"]);
            }
            let path = fixture.generate(&args, "bounded.mkv");
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { media.fill_initial_queues() }.unwrap();
            if case != "audio-tail" {
                assert!(media.video_queue.is_empty());
                assert!(unsafe { media.audio_full() });
                let info = unsafe { VideoInfo::inspect(&media, ptr::null()) };
                assert!(info.lines[1].contains("64X64"));
            }
            if case == "deferred-fallback" {
                // Force the same initial rejection as a hardware decoder that
                // cannot produce Vulkan frames, without requiring a GPU.
                unsafe { media.select_audio_track(1, 0.5) }.unwrap();
                media.video.uses_vulkan = true;
                let worker = DecodeWorker::start(media, path, false);
                let mut frames = VecDeque::new();
                let mut recovered = false;
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline {
                    worker.check_error().unwrap();
                    worker.receive_frames(&mut frames);
                    frames.clear();
                    {
                        let media = worker.lock().unwrap();
                        unsafe { ffi::up_audio_stream_clear(media.audio.as_ref().unwrap().stream) };
                        if !media.video.uses_vulkan && media.first_video_pts.is_some() {
                            assert_eq!(
                                media.selected_audio_track, 1,
                                "fallback changed the audio track"
                            );
                            recovered = true;
                            break;
                        }
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                assert!(recovered, "initial video decoding must retry in software");
                return;
            }
            for _ in 0..128 {
                unsafe { media.fill_queues() }.unwrap();
            }
            assert!(
                !media.eof,
                "demuxing must wait for buffered audio to be consumed"
            );
            let audio = media.audio.as_ref().unwrap();
            let queued = unsafe { audio.queued_bytes() };
            assert!(
                (AUDIO_QUEUE_MAX_BYTES..AUDIO_QUEUE_MAX_BYTES + 16_384).contains(&queued),
                "queued bytes: {queued}"
            );
            for _ in 0..256 {
                // Simulate the device consuming the buffered audio.
                unsafe { ffi::up_audio_stream_clear(media.audio.as_ref().unwrap().stream) };
                media.video_queue.clear();
                unsafe { media.fill_queues() }.unwrap();
                if media.eof {
                    break;
                }
            }
            assert!(media.eof);
            assert!(media.first_video_pts.is_some());
            assert_eq!(
                media.audio.as_ref().unwrap().submitted_frames,
                8 * i64::from(AUDIO_RATE)
            );
        }
        "delayed-audio" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=25:duration=6",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=48000:duration=6",
                    "-itsoffset",
                    "3",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=48000:duration=3",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-map",
                    "2:a",
                    "-c:v",
                    "mpeg4",
                    "-g",
                    "25",
                    "-c:a",
                    "pcm_s16le",
                ],
                "delayed.mkv",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { media.select_audio_track(1, 0.5) }.unwrap();
            let mut anchor = None;
            for _ in 0..128 {
                unsafe { media.fill_queues() }.unwrap();
                anchor =
                    unsafe { media.finish_seek(media.video_queue.front().map(|f| f.pts), 0.5) };
                if anchor.is_some() {
                    break;
                }
            }
            let anchor =
                anchor.expect("full video buffer must allow seek completion without audio");
            assert!((0.46..0.55).contains(&anchor), "seek anchor: {anchor}");
            assert!(unsafe { media.audio_clock() }.is_none());
            unsafe { media.sync_audio(anchor, false) }.unwrap();
            assert!(!media.audio.as_ref().unwrap().resumed);
            // Advance video until the delayed audio is demuxed.
            for step in 0..150 {
                let now = anchor + f64::from(step) / 50.0;
                while media.video_queue.front().is_some_and(|f| f.pts <= now) {
                    media.video_queue.pop_front();
                }
                unsafe { media.fill_queues() }.unwrap();
                unsafe { media.sync_audio(now, true) }.unwrap();
                assert!(
                    !media.audio.as_ref().unwrap().resumed,
                    "paused seek must stay silent"
                );
                unsafe { media.sync_audio(now, false) }.unwrap();
                if now < 3.0 {
                    assert!(!media.audio.as_ref().unwrap().resumed);
                }
            }
            let audio = media.audio.as_ref().unwrap();
            assert!((audio.first_pts.unwrap() - 3.0).abs() < 0.001);
            assert!(audio.resumed);
        }
        _ => panic!("unknown regression case"),
    }
}
