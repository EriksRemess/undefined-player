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
fn bounded_decode_preserves_frames_under_backpressure() {
    child("bounded-decode");
}
#[test]
fn audio_is_buffered_while_video_is_backpressured() {
    child("audio-priority");
}
#[test]
fn seeking_restores_full_decoding_after_keyframe_catchup() {
    child("catchup-seek");
}
#[test]
fn embedded_chapters_use_the_normal_seek_path() {
    child("chapters");
}
#[test]
fn embedded_artwork_is_extracted_and_cleaned_up() {
    child("artwork");
}
#[test]
fn delayed_audio_switch_makes_progress() {
    child("delayed-audio");
}
#[test]
fn worker_seek_completes_before_delayed_audio() {
    child("worker-delayed-audio-seek");
}
#[test]
fn decoded_frames_preserve_interlacing_and_field_order() {
    child("interlacing");
}
#[test]
fn pictures_override_incorrect_interlacing_flags() {
    child("picture-interlacing");
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
fn muting_audio_changes_gain_without_pausing() {
    child("mute");
}
#[test]
fn buffered_file_seeks_preserve_pictures_and_eof() {
    child("buffered-file");
}
#[test]
fn delayed_video_does_not_block_startup() {
    child("delayed-video");
}

#[test]
fn hls_master_selects_highest_quality() {
    child("hls-quality");
}

#[test]
fn hls_audio_tracks_can_be_reselected_after_discard() {
    child("hls-audio-selection");
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

#[test]
fn native_geometry_does_not_leave_zoom_waiting_for_pixels() {
    child("native-geometry");
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
    assert_ne!(unsafe { ffi::up_platform_init() }, 0, "{}", sdl_error());
    let _sdl = Sdl;
    let fixture = Fixture::new();
    match case.as_str() {
        "native-geometry" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x64:r=1:d=1",
                    "-c:v",
                    "ffv1",
                ],
                "geometry.mkv",
            );
            let media = unsafe { Media::open(&path, None, false) }.unwrap();
            let raw = unsafe { ffi::up_av_decoder_placeholder(media.video.as_ptr()) };
            assert!(!raw.is_null());
            let frame = unsafe { crate::decoder::VideoFrame::from_raw(raw, 0.0, 1.0) };
            assert_eq!(frame.dimensions(), (64, 64));
            let mut crop = crate::autocrop::AutoCrop::default();
            assert!(crop.toggle());
            crop.update(&frame, false);
            assert!(crop.unavailable());
            let mut zoom = crate::zoom::Zoom::default();
            zoom.toggle(crop.enabled());
            assert!(zoom.fill(crop.enabled(), crop.crop().is_some(), crop.unavailable()));
            crop.reset();
            crop.update(&frame, true);
            assert!(crop.unavailable());
        }
        "worker-delayed-audio-seek" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x64:r=30:d=14",
                    "-itsoffset",
                    "12",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=48000:duration=2",
                    "-c:v",
                    "ffv1",
                    "-c:a",
                    "pcm_s16le",
                ],
                "delayed-worker.mkv",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { media.seek(0.5) }.unwrap();
            let worker = DecodeWorker::start(media, crate::source::MediaSource::File(path), false);
            let mut frames = VecDeque::new();
            let mut anchor = None;
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                worker.check_error().unwrap();
                worker.receive_frames(&mut frames);
                if let Some(media) = worker.try_lock().unwrap() {
                    anchor = unsafe {
                        media.finish_seek(
                            frames.front().map(|f| f.frame.pts),
                            worker.pending_frames(),
                            0.5,
                        )
                    };
                    if anchor.is_some() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
            frames.clear();
            drop(worker);
            let anchor = anchor.expect(
                "seek waits forever when video is buffered outside Media and audio starts later",
            );
            assert!((0.46..0.55).contains(&anchor), "seek anchor: {anchor}");
        }
        "catchup-seek" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x64:r=30:d=2",
                    "-c:v",
                    "mpeg4",
                    "-g",
                    "30",
                    "-bf",
                    "0",
                ],
                "catchup.mkv",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { ffi::up_av_decoder_skip_nonkey(media.video.as_ptr(), 1) };
            while unsafe { ffi::up_av_read_frame(media.format, media.packet) } >= 0 {
                unsafe { media.decode_packet() }.unwrap();
                unsafe { ffi::up_av_packet_unref(media.packet) };
            }
            unsafe { media.drain() }.unwrap();
            assert_eq!(
                media.video_queue.len(),
                2,
                "catch-up should decode only the two keyframes"
            );
            unsafe { media.seek(0.5) }.unwrap();
            let mut after_catchup = Vec::new();
            while !media.eof {
                unsafe { media.fill_queues() }.unwrap();
                while let Some(frame) = media.video_queue.pop_front() {
                    after_catchup.push(frame.pts);
                }
            }
            let mut baseline = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { baseline.seek(0.5) }.unwrap();
            let mut expected = Vec::new();
            while !baseline.eof {
                unsafe { baseline.fill_queues() }.unwrap();
                expected.extend(baseline.video_queue.drain(..).map(|frame| frame.pts));
            }
            assert!(expected.len() >= 45);
            assert_eq!(
                after_catchup, expected,
                "seek did not restore normal dependent-frame decoding"
            );
        }
        "audio-priority" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x64:r=60:d=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=44100:duration=2",
                    "-c:v",
                    "ffv1",
                    "-c:a",
                    "aac",
                ],
                "audio-priority.mkv",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            for _ in 0..100 {
                unsafe { media.fill_queues_step(0) }.unwrap();
                assert!(
                    media.video_queue.is_empty(),
                    "video backpressure was ignored"
                );
                if unsafe { media.audio.as_ref().unwrap().queued_bytes() }
                    >= AUDIO_QUEUE_TARGET_BYTES
                {
                    break;
                }
            }
            assert!(
                unsafe { media.audio.as_ref().unwrap().queued_bytes() } >= AUDIO_QUEUE_TARGET_BYTES,
                "video backpressure prevented audio from being buffered"
            );
            assert!(!media.eof);
            let mut timestamps = Vec::new();
            for _ in 0..2000 {
                unsafe { ffi::up_audio_stream_clear(media.audio.as_ref().unwrap().stream) };
                unsafe { media.fill_queues_step(1) }.unwrap();
                assert!(media.video_queue.len() <= 1);
                if let Some(frame) = media.video_queue.pop_front() {
                    timestamps.push(frame.pts);
                }
                if media.eof {
                    break;
                }
            }
            assert_eq!(
                timestamps.len(),
                120,
                "compressed read-ahead lost video at EOF"
            );
            assert!(timestamps.windows(2).all(|times| times[1] > times[0]));
            unsafe { media.seek(1.0) }.unwrap();
            unsafe { media.fill_queues() }.unwrap();
            assert!(
                media.video_queue.front().unwrap().pts >= 0.99,
                "seeking retained a stale compressed packet"
            );
        }
        "bounded-decode" => {
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x64:r=12",
                    "-t",
                    "1",
                    "-c:v",
                    "ffv1",
                ],
                "bounded.mkv",
            );
            let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
            unsafe { media.fill_queues_step(0) }.unwrap();
            assert!(media.video_queue.is_empty() && !media.eof);
            let mut timestamps = Vec::new();
            for _ in 0..100 {
                unsafe { media.fill_queues_step(1) }.unwrap();
                assert!(
                    media.video_queue.len() <= 1,
                    "backpressure exceeded the available slot"
                );
                if let Some(frame) = media.video_queue.pop_front() {
                    timestamps.push(frame.pts);
                }
                if media.eof && media.video_queue.is_empty() {
                    break;
                }
            }
            assert_eq!(timestamps.len(), 12, "bounded handoff lost decoded frames");
            assert!(timestamps.windows(2).all(|times| times[1] > times[0]));
        }
        "mute" => {
            let control = AudioControl::default();
            let mut audio = unsafe { AudioOutput::create(control.clone()) }.unwrap();
            unsafe { audio.set_paused(false) }.unwrap();
            assert!(audio.resumed);
            assert!(!control.is_muted());
            let thread_control = control.clone();
            std::thread::spawn(move || thread_control.set_muted(true))
                .join()
                .unwrap()
                .unwrap();
            assert!(control.is_muted());
            assert!(audio.resumed, "muting must not pause or stall audio");
            control.set_muted(false).unwrap();
            assert!(!control.is_muted());
            assert!(audio.resumed);
        }
        "hls-audio-selection" => {
            fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "color=black:s=64x64:r=5",
                    "-t",
                    "2",
                    "-c:v",
                    "mpeg2video",
                    "-g",
                    "5",
                    "-f",
                    "hls",
                    "-hls_time",
                    "1",
                ],
                "video.m3u8",
            );
            fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=r=48000:cl=stereo",
                    "-t",
                    "2",
                    "-c:a",
                    "aac",
                    "-f",
                    "hls",
                    "-hls_time",
                    "1",
                ],
                "audio.m3u8",
            );
            std::fs::copy(fixture.0.join("audio.m3u8"), fixture.0.join("other.m3u8")).unwrap();
            std::fs::write(
                fixture.0.join("captions.vtt"),
                "WEBVTT\n\n00:00:01.800 --> 00:00:02.000\nCaption fixture\n",
            )
            .unwrap();
            std::fs::write(
                fixture.0.join("captions.m3u8"),
                "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\ncaptions.vtt\n#EXT-X-ENDLIST\n",
            )
            .unwrap();
            let master = fixture.0.join("master.m3u8");
            std::fs::write(&master, concat!(
                "#EXTM3U\n",
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"\n",
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"French\",LANGUAGE=\"fr\",DEFAULT=NO,URI=\"other.m3u8\"\n",
                "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"s\",NAME=\"English\",LANGUAGE=\"en\",URI=\"captions.m3u8\"\n",
                "#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=64x64,AUDIO=\"a\",SUBTITLES=\"s\"\nvideo.m3u8\n",
            )).unwrap();
            let mut media = unsafe { Media::open(&master, None, false) }.unwrap();
            assert_eq!(media.audio_tracks.len(), 2);
            assert_eq!(media.subtitle_tracks.len(), 1);
            for track in [0, 1, 0] {
                unsafe { media.select_audio_track(track, 1.6) }.unwrap();
                let wanted = media.audio_tracks[track].decoder.stream_index;
                let unwanted = media.audio_tracks[1 - track].decoder.stream_index;
                let mut packets = 0;
                loop {
                    let result = unsafe { ffi::up_av_read_frame(media.format, media.packet) };
                    if result < 0 {
                        assert!(crate::decoder::ffmpeg_error_is_eof(result));
                        break;
                    }
                    let index = unsafe { ffi::up_av_packet_stream_index(media.packet) };
                    assert_ne!(index, unwanted, "discarded audio must not be read");
                    packets += usize::from(index == wanted);
                    if index == media.subtitle_tracks[0].decoder.stream_index {
                        unsafe { media.decode_packet() }.unwrap();
                    }
                    unsafe { ffi::up_av_packet_unref(media.packet) };
                }
                assert!(packets > 0, "selected audio must be readable after seeking");
                assert!(
                    !media.subtitle_queue.is_empty(),
                    "HLS subtitle cues must be decoded after seeking"
                );
            }
        }
        "hls-quality" => {
            for (name, size, color) in [
                ("low.ts", "64x64", "black"),
                ("high-bandwidth.ts", "64x64", "white"),
                ("large.ts", "96x64", "gray"),
            ] {
                fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        &format!("color={color}:size={size}:rate=5:duration=1"),
                        "-c:v",
                        "mpeg2video",
                        "-f",
                        "mpegts",
                    ],
                    name,
                );
                let playlist = format!(
                    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1.0,\n{name}\n#EXT-X-ENDLIST\n"
                );
                std::fs::write(fixture.0.join(name.replace(".ts", ".m3u8")), playlist).unwrap();
            }

            let open_first_frame = |master: &str| {
                let path = fixture.0.join("master.m3u8");
                std::fs::write(&path, master).unwrap();
                let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
                unsafe { media.fill_initial_queues() }.unwrap();
                let frame = media.video_queue.pop_front().expect("decoded HLS frame");
                (
                    frame.dimensions(),
                    crate::luma::Luma::new(&frame).unwrap().sample(0, 0),
                )
            };

            let (dimensions, _) = open_first_frame(
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=64x64\nhigh-bandwidth.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=50000,RESOLUTION=96x64\nlarge.m3u8\n",
            );
            assert_eq!(dimensions, (96, 64), "largest rendition must win");

            let (dimensions, luma) = open_first_frame(
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=64x64\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=64x64\nhigh-bandwidth.m3u8\n",
            );
            assert_eq!(dimensions, (64, 64));
            assert!(luma > 200, "higher-bandwidth rendition must win: {luma}");
        }
        "buffered-file" => {
            // Larger than the entire read-ahead/back buffer. Each frame has a
            // distinct luma value, so stale bytes after a seek cannot pass by
            // merely receiving a plausible timestamp. Include URL punctuation
            // and Unicode in a relative filename as well.
            let path = fixture.generate(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=size=320x240:rate=25:duration=8,geq=lum='16+N':cb=128:cr=128",
                    "-c:v",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                ],
                "buffered: gāčē %20 #?.avi",
            );
            assert!(std::fs::metadata(&path).unwrap().len() > 16 * 1024 * 1024);
            env::set_current_dir(&fixture.0).unwrap();
            let mut media = unsafe {
                Media::open(std::path::Path::new(path.file_name().unwrap()), None, false)
            }
            .unwrap();
            // Read through the entire ring several times before seeking. Check
            // every decoded picture, not just demux timestamps or byte counts.
            let mut decoded = 0;
            while !media.eof || !media.video_queue.is_empty() {
                unsafe { media.fill_initial_queues() }.unwrap();
                for frame in media.video_queue.drain(..) {
                    let luma = crate::luma::Luma::new(&frame).unwrap();
                    assert_eq!(luma.sample(0, 0), 16 + decoded as u8);
                    decoded += 1;
                }
            }
            assert_eq!(decoded, 200);
            // Cross the buffer in both directions, then rewind after EOF.
            for target in [0.0, 6.0, 0.4, 0.8, 0.6, 7.8, 0.0] {
                unsafe { media.seek(target) }.unwrap();
                unsafe { media.fill_initial_queues() }.unwrap();
                let frame = media.video_queue.front().expect("seek produced a frame");
                assert!(
                    (frame.pts - target).abs() < 0.001,
                    "{} != {target}",
                    frame.pts
                );
                let luma = crate::luma::Luma::new(frame).unwrap();
                assert_eq!(luma.sample(0, 0), 16 + (target * 25.0).round() as u8);
                if target == 7.8 {
                    let mut frames = 0;
                    while !media.eof || !media.video_queue.is_empty() {
                        frames += media.video_queue.len();
                        media.video_queue.clear();
                        unsafe { media.fill_initial_queues() }.unwrap();
                    }
                    assert_eq!(frames, 5);
                }
            }
            drop(media); // Also close with unread background data queued.
            env::set_current_dir(env::temp_dir()).unwrap();
        }
        "artwork" => {
            for (source_width, source_height, width, height) in [
                (64, 96, 341, 512),
                (1020, 1024, 510, 512),
                (1016, 1024, 508, 512),
                (1024, 1020, 512, 510),
            ] {
                let cover = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        &format!("color=red:size={source_width}x{source_height}"),
                        "-frames:v",
                        "1",
                        "-update",
                        "1",
                    ],
                    &format!("cover-{source_width}-{source_height}.jpg"),
                );
                let path = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=64x64:rate=25:duration=0.4",
                        "-i",
                        cover.to_str().unwrap(),
                        "-map",
                        "0:v",
                        "-map",
                        "1:v",
                        "-c:v:0",
                        "mpeg4",
                        "-c:v:1",
                        "copy",
                        "-disposition:v:1",
                        "attached_pic",
                    ],
                    &format!("artwork-{source_width}-{source_height}.mp4"),
                );
                let media = unsafe { Media::open(&path, None, false) }.unwrap();
                let artwork = unsafe { crate::artwork::Artwork::extract(media.format) }.unwrap();
                let pixels = Command::new("ffmpeg")
                    .args(["-v", "error", "-i"])
                    .arg(&artwork.path)
                    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                    .output()
                    .unwrap();
                assert!(
                    pixels.status.success(),
                    "{}",
                    String::from_utf8_lossy(&pixels.stderr)
                );
                assert_eq!(pixels.stdout.len(), 512 * 512 * 4);
                let alpha = |x: usize, y: usize| pixels.stdout[(y * 512 + x) * 4 + 3];
                let left = (512 - width) / 2;
                let top = (512 - height) / 2;
                for y in 0..512 {
                    for x in 0..512 {
                        let expected = if (left..left + width).contains(&x)
                            && (top..top + height).contains(&y)
                        {
                            255
                        } else {
                            0
                        };
                        assert_eq!(
                            alpha(x, y),
                            expected,
                            "{source_width}x{source_height}: pixel {x},{y}"
                        );
                    }
                }
                let center = (256 * 512 + 256) * 4;
                assert!(pixels.stdout[center] >= 250);
                assert!(pixels.stdout[center + 1] <= 5 && pixels.stdout[center + 2] <= 5);
                let opaque_width = (0..512).filter(|&x| alpha(x, 256) > 0).count();
                assert_eq!(opaque_width, width);
                let saved_path = artwork.path.clone();
                drop(artwork);
                assert!(!saved_path.exists());
            }
            assert!(unsafe { crate::artwork::Artwork::extract(ptr::null()) }.is_none());
        }
        "chapters" => {
            let metadata = fixture.0.join("chapters.txt");
            std::fs::write(&metadata, ";FFMETADATA1\ntitle=Example Movie\nartist=Example Artist\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=800\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=800\nEND=1600\ntitle=Second\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1600\nEND=2400\ntitle=Third\n").unwrap();
            for (extension, rate) in [("mp4", 25), ("mkv", 25), ("mp4", 5), ("mkv", 5)] {
                let path = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        &format!("testsrc2=size=64x64:rate={rate}:duration=2.4"),
                        "-f",
                        "ffmetadata",
                        "-i",
                        metadata.to_str().unwrap(),
                        "-map",
                        "0:v",
                        "-map_chapters",
                        "1",
                        "-map_metadata",
                        "1",
                        "-c:v",
                        "mpeg4",
                        "-g",
                        "12",
                    ],
                    &format!("chapters-{rate}.{extension}"),
                );
                let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
                unsafe { media.fill_initial_queues() }.unwrap();
                let metadata = unsafe { crate::metadata::MediaMetadata::inspect(media.format) };
                assert_eq!(
                    metadata.overlay_text(None).to_str().unwrap(),
                    "TITLE: EXAMPLE MOVIE\nARTIST: EXAMPLE ARTIST"
                );
                let origin = media.first_video_pts.unwrap();
                let duration = media.duration();
                let mut chapters =
                    unsafe { crate::chapters::Chapters::inspect(media.format, origin, duration) };
                assert_eq!(unsafe { ffi::up_av_chapter_count(media.format) }, 3);
                assert!(unsafe { ffi::up_av_chapter_start(media.format, 3) }.is_nan());
                assert_eq!(chapters.markers(duration).len(), 3);
                assert_eq!(
                    chapters.label(1).unwrap().to_str().unwrap(),
                    "CHAPTER 2 / 3 — SECOND"
                );
                assert!(unsafe { ffi::up_av_chapter_title(media.format, 3) }.is_null());
                let mut clock = WallClock::new(origin);
                clock.set_paused(true);
                let mut notice = crate::presentation::PositionNotice::new();
                for (forward, expected) in [(true, 0.8), (true, 1.6), (false, 0.8), (false, 0.0)] {
                    let target = chapters
                        .target(chapters.playback_position(clock.now() - origin), forward)
                        .unwrap();
                    assert!((target - expected).abs() < 0.001);
                    unsafe {
                        crate::playback::seek_to(
                            &mut media,
                            &mut clock,
                            &mut notice,
                            target,
                            origin,
                            duration,
                        )
                    }
                    .unwrap();
                    unsafe { media.fill_initial_queues() }.unwrap();
                    let frame = media.video_queue.front().unwrap();
                    assert!(
                        (frame.pts - (origin + target)).abs() <= 1.0 / f64::from(rate) + 0.001,
                        "{}: {}",
                        extension,
                        frame.pts
                    );
                    chapters.seeked(target);
                    let landed =
                        unsafe { media.finish_seek(Some(frame.pts), 0, origin + target) }.unwrap();
                    clock.seek(landed);
                    assert_eq!(
                        chapters.current(chapters.playback_position(clock.now() - origin)),
                        chapters.current(target)
                    );
                }
            }
        }
        "picture-interlacing" => {
            for (name, rate, filter, expected) in [
                ("film", 25, "setfield=tff", 0),
                ("top", 50, "interlace=scan=tff", 1),
                ("bottom", 50, "interlace=scan=bff", 2),
            ] {
                let path = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        &format!("testsrc2=size=320x240:rate={rate}:duration=2"),
                        "-vf",
                        filter,
                        "-c:v",
                        "mpeg2video",
                        "-flags",
                        "+ildct+ilme",
                    ],
                    &format!("detect-{name}.mkv"),
                );
                let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
                let mut seen = 0;
                let mut matching = 0;
                while !media.eof || !media.video_queue.is_empty() {
                    unsafe { media.fill_initial_queues() }.unwrap();
                    while let Some(frame) = media.video_queue.pop_front() {
                        let flagged = unsafe { ffi::up_av_frame_field(frame.as_ptr()) };
                        assert_ne!(flagged, 0, "fixture must be flagged interlaced");
                        if seen >= 10 {
                            matching += usize::from(
                                crate::deinterlace::Mode::Auto.frame_field(&frame) == expected,
                            );
                        }
                        assert_eq!(crate::deinterlace::Mode::Off.frame_field(&frame), 0);
                        assert_eq!(crate::deinterlace::Mode::On.frame_field(&frame), flagged);
                        assert_eq!(
                            frame.clone_reference().unwrap().detected_field,
                            frame.detected_field
                        );
                        seen += 1;
                    }
                }
                assert!(seen >= 40, "{name}: only {seen} frames");
                assert!(
                    matching * 10 >= (seen - 10) * 9,
                    "{name}: {matching}/{} decisions correct",
                    seen - 10
                );
                unsafe { media.seek(0.0) }.unwrap();
                unsafe { media.fill_initial_queues() }.unwrap();
                assert!(
                    media.video_queue.front().unwrap().detected_field.is_none(),
                    "seek must discard previous decisions"
                );
            }
        }
        "interlacing" => {
            for (name, filter, flags, expected) in [
                ("progressive", "null", "0", 0),
                ("top", "interlace=scan=tff", "+ildct+ilme", 1),
                ("bottom", "interlace=scan=bff", "+ildct+ilme", 2),
            ] {
                let path = fixture.generate(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=64x64:rate=50:duration=0.4",
                        "-vf",
                        filter,
                        "-c:v",
                        "mpeg2video",
                        "-flags",
                        flags,
                    ],
                    &format!("{name}.mkv"),
                );
                let mut media = unsafe { Media::open(&path, None, false) }.unwrap();
                unsafe { media.fill_initial_queues() }.unwrap();
                assert!(!media.video_queue.is_empty());
                for frame in &media.video_queue {
                    assert_eq!(
                        unsafe { ffi::up_av_frame_field(frame.as_ptr()) },
                        expected,
                        "{name}"
                    );
                }
            }
        }
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
                // A resume prompt must be drawable while this audio remains
                // paused: the decoder cannot produce its first picture yet.
                let background =
                    crate::playback::resume_background(true, !media.video_queue.is_empty())
                        .unwrap()
                        .unwrap();
                assert_eq!(background.dimensions(), (16, 16));
                let info = unsafe { VideoInfo::inspect(&media, ptr::null()) };
                assert!(info.lines[1].contains("64X64"));
            }
            if case == "deferred-fallback" {
                // Force the same initial rejection as a hardware decoder that
                // cannot produce Vulkan frames, without requiring a GPU.
                unsafe { media.select_audio_track(1, 0.5) }.unwrap();
                media.video.uses_hardware = true;
                let worker =
                    DecodeWorker::start(media, crate::source::MediaSource::File(path), false);
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
                        if !media.video.uses_hardware && media.first_video_pts.is_some() {
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
                    unsafe { media.finish_seek(media.video_queue.front().map(|f| f.pts), 0, 0.5) };
                if anchor.is_some() {
                    break;
                }
            }
            let anchor =
                anchor.expect("full video buffer must allow seek completion without audio");
            assert!((0.46..0.55).contains(&anchor), "seek anchor: {anchor}");
            // Compressed read-ahead may already have found the delayed track.
            // Its timestamp must still keep it silent until 3 seconds.
            assert!(unsafe { media.audio_clock() }.is_none_or(|pts| (pts - 3.0).abs() < 0.001));
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
