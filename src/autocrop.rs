//! Opt-in black-bar detection. A bounded worker samples frame references; crop
//! decisions and stability stay in Rust, off the presentation/GPU render path.
use crate::{decoder::VideoFrame, ffi};
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) type Crop = ffi::UpVideoCrop;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

struct Luma(ffi::UpLumaView);
impl Luma {
    fn new(frame: &VideoFrame) -> Option<Self> {
        let mut view = ffi::UpLumaView::default();
        (unsafe { ffi::up_av_frame_luma(frame.as_ptr(), &mut view) } != 0).then_some(Self(view))
    }
    fn sample(&self, x: usize, y: usize) -> u8 {
        let v = &self.0;
        // The adapter validates the component layout and row length. This
        // reference owns the AVFrame, including negative-stride plane storage.
        let p = unsafe {
            v.data
                .offset(y as isize * v.stride as isize)
                .add(x * v.step as usize)
        };
        let value = if v.depth + v.shift <= 8 {
            u16::from(unsafe { *p })
        } else {
            let bytes = unsafe { [*p, *p.add(1)] };
            if v.big_endian != 0 {
                u16::from_be_bytes(bytes)
            } else {
                u16::from_le_bytes(bytes)
            }
        };
        let component = (u32::from(value) >> v.shift) & ((1_u32 << v.depth) - 1);
        (component >> (v.depth - 8)) as u8
    }
    fn detect(&self) -> Option<Crop> {
        detect(
            self.0.width as usize,
            self.0.height as usize,
            if self.0.full_range != 0 { 8 } else { 24 },
            |x, y| self.sample(x, y),
        )
    }
}
impl Drop for Luma {
    fn drop(&mut self) {
        unsafe { ffi::up_av_frame_luma_free(&mut self.0) };
    }
}

fn detect(
    width: usize,
    height: usize,
    black: u8,
    pixel: impl Fn(usize, usize) -> u8,
) -> Option<Crop> {
    if width < 16 || height < 16 {
        return None;
    }
    let row_has_content = |y| (0..64).any(|i| pixel(i * (width - 1) / 63, y) > black);
    let column_has_content = |x| (0..64).any(|i| pixel(x, i * (height - 1) / 63) > black);
    let top = (0..height).find(|&y| row_has_content(y))?;
    let bottom = (top..height).rfind(|&y| row_has_content(y))? + 1;
    let left = (0..width).find(|&x| column_has_content(x))?;
    let right = (left..width).rfind(|&x| column_has_content(x))? + 1;
    // Reject small highlights without excluding pillarboxed portrait pictures
    // or wide letterboxed content. Require substantial area and one long axis;
    // the brightness check below still rejects fades and very dark scenes.
    let content_width = right - left;
    let content_height = bottom - top;
    let content_area = content_width as u64 * content_height as u64;
    let frame_area = width as u64 * height as u64;
    if content_area * 5 < frame_area || (content_width < width / 2 && content_height < height / 2) {
        return None;
    }
    let bright = (0..16)
        .flat_map(|y| (0..16).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            pixel(
                left + x * (right - left - 1) / 15,
                top + y * (bottom - top - 1) / 15,
            ) > black.saturating_add(16)
        })
        .count();
    if bright < 32 {
        return None;
    }
    // Round outward, retaining edge pixels and aligning subsampled chroma.
    Some(Crop {
        width: width as i32,
        height: height as i32,
        left: if left <= 4 { 0 } else { (left & !1) as i32 },
        top: if top <= 4 { 0 } else { (top & !1) as i32 },
        right: if width - right <= 4 {
            width
        } else {
            (right + 1) & !1
        } as i32,
        bottom: if height - bottom <= 4 {
            height
        } else {
            (bottom + 1) & !1
        } as i32,
    })
}

fn union(a: Crop, b: Crop) -> Crop {
    Crop {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
        ..a
    }
}
fn close(a: Crop, b: Crop) -> bool {
    a.width == b.width
        && a.height == b.height
        && [
            (a.left, b.left),
            (a.top, b.top),
            (a.right, b.right),
            (a.bottom, b.bottom),
        ]
        .iter()
        .all(|(a, b)| (a - b).abs() <= 4)
}
#[derive(Default)]
struct Tracker {
    crop: Option<Crop>,
    history: VecDeque<Crop>,
}
impl Tracker {
    fn observe(&mut self, sample: Option<Crop>, paused: bool) {
        let Some(sample) = sample else {
            self.history.clear();
            return;
        };
        if self
            .crop
            .is_some_and(|c| (c.width, c.height) != (sample.width, sample.height))
        {
            *self = Self::default();
        }
        // Reveal content immediately when an aspect change reaches outside the
        // current crop. Only tightening a crop needs repeated stable samples.
        if let Some(current) = self.crop {
            self.crop = Some(union(current, sample));
        }
        if self
            .history
            .iter()
            .any(|previous| !close(*previous, sample))
        {
            self.history.clear();
        }
        self.history.push_back(sample);
        if self.history.len() > 3 {
            self.history.pop_front();
        }
        if paused || self.history.len() == 3 {
            self.crop = self.history.iter().copied().reduce(union);
        }
    }
}

struct Probe {
    generation: u64,
    frame: VideoFrame,
    paused: bool,
}
struct Answer {
    generation: u64,
    crop: Result<Option<Crop>, ()>,
    paused: bool,
}
struct Worker {
    sender: Option<SyncSender<Probe>>,
    answers: Receiver<Answer>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    fn start() -> std::io::Result<Self> {
        let (sender, probes) = mpsc::sync_channel::<Probe>(1);
        let (results, answers) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("autocrop".into())
            .spawn(move || {
                while let Ok(probe) = probes.recv() {
                    let crop = Luma::new(&probe.frame).map(|luma| luma.detect()).ok_or(());
                    if results
                        .send(Answer {
                            generation: probe.generation,
                            crop,
                            paused: probe.paused,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            answers,
            thread: Some(thread),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct AutoCrop {
    enabled: bool,
    unavailable: bool,
    generation: u64,
    last_sample: Option<Instant>,
    last_pts: Option<f64>,
    sampled_pts: Option<(f64, bool)>,
    tracker: Tracker,
    worker: Option<Worker>,
}
impl AutoCrop {
    pub(crate) fn toggle(&mut self) -> bool {
        if !self.enabled && self.worker.is_none() {
            match Worker::start() {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    eprintln!("warning: could not start autocrop: {error}");
                    return false;
                }
            }
        }
        self.enabled = !self.enabled;
        self.reset();
        eprintln!("autocrop: {}", if self.enabled { "on" } else { "off" });
        self.enabled
    }
    // Also called explicitly before playback clears frames for any seek or
    // audio-track change. PTS gaps alone cannot identify short forward seeks.
    pub(crate) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.tracker = Tracker::default();
        self.last_sample = None;
        self.last_pts = None;
        self.sampled_pts = None;
        self.unavailable = false;
    }
    pub(crate) fn crop(&self) -> Option<Crop> {
        self.tracker.crop.filter(|_| self.enabled)
    }
    fn accept_answer(&mut self, answer: Answer) {
        if !self.enabled || answer.generation != self.generation {
            return;
        }
        match answer.crop {
            Ok(crop) => self.tracker.observe(crop, answer.paused),
            Err(()) => {
                if !self.unavailable {
                    eprintln!("warning: autocrop cannot read this video pixel format");
                }
                self.unavailable = true;
            }
        }
    }
    pub(crate) fn update(&mut self, frame: &VideoFrame, paused: bool) -> bool {
        let previous = self.crop();
        if self
            .last_pts
            .is_some_and(|pts| frame.pts < pts || frame.pts - pts > 2.0)
        {
            self.reset();
        }
        self.last_pts = Some(frame.pts);
        while let Some(answer) = self.worker.as_ref().and_then(|w| w.answers.try_recv().ok()) {
            self.accept_answer(answer);
        }
        let Some(worker) = &self.worker else {
            return false;
        };
        if self.enabled
            && !self.unavailable
            && self
                .last_sample
                .is_none_or(|time| time.elapsed() >= SAMPLE_INTERVAL)
            && !(paused && self.sampled_pts == Some((frame.pts, true)))
            && let Some(sample) = frame.clone_reference()
            && worker
                .sender
                .as_ref()
                .unwrap()
                .try_send(Probe {
                    generation: self.generation,
                    frame: sample,
                    paused,
                })
                .is_ok()
        {
            self.last_sample = Some(Instant::now());
            self.sampled_pts = Some((frame.pts, paused));
        }
        let changed = previous != self.crop();
        if changed && let Some(crop) = self.crop() {
            eprintln!(
                "autocrop: {}x{} at {},{}",
                crop.right - crop.left,
                crop.bottom - crop.top,
                crop.left,
                crop.top
            );
        }
        changed
    }
    pub(crate) fn shutdown(&mut self) {
        self.worker.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(left: usize, top: usize, right: usize, bottom: usize) -> Crop {
        detect(720, 480, 24, |x, y| {
            if (left..right).contains(&x) && (top..bottom).contains(&y) {
                160
            } else {
                16
            }
        })
        .unwrap()
    }
    #[test]
    fn detects_letterbox_pillarbox_and_rounds_outward() {
        assert_eq!(
            bars(0, 113, 720, 371),
            Crop {
                width: 720,
                height: 480,
                left: 0,
                top: 112,
                right: 720,
                bottom: 372
            }
        );
        assert_eq!(
            bars(91, 0, 631, 480),
            Crop {
                width: 720,
                height: 480,
                left: 90,
                top: 0,
                right: 632,
                bottom: 480
            }
        );
        assert_eq!(
            bars(80, 60, 640, 420),
            Crop {
                width: 720,
                height: 480,
                left: 80,
                top: 60,
                right: 640,
                bottom: 420
            }
        );
        assert_eq!(
            bars(0, 0, 720, 480),
            Crop {
                width: 720,
                height: 480,
                left: 0,
                top: 0,
                right: 720,
                bottom: 480
            }
        );
        assert!(detect(720, 480, 24, |_, _| 16).is_none());
        assert!(
            detect(720, 480, 24, |x, y| {
                if (300..400).contains(&x) && (200..300).contains(&y) {
                    180
                } else {
                    16
                }
            })
            .is_none()
        );
        assert!(detect(720, 480, 24, |_, _| 30).is_none());
    }

    #[test]
    fn narrow_pictures_are_not_mistaken_for_dark_scene_highlights() {
        for expected in [
            Crop {
                width: 1920,
                height: 1080,
                left: 656,
                top: 0,
                right: 1264,
                bottom: 1080,
            },
            Crop {
                width: 1920,
                height: 1080,
                left: 0,
                top: 360,
                right: 1920,
                bottom: 720,
            },
            Crop {
                width: 1920,
                height: 1080,
                left: 672,
                top: 28,
                right: 1248,
                bottom: 1052,
            },
        ] {
            let actual = detect(
                expected.width as usize,
                expected.height as usize,
                24,
                |x, y| {
                    if (expected.left..expected.right).contains(&(x as i32))
                        && (expected.top..expected.bottom).contains(&(y as i32))
                    {
                        160
                    } else {
                        16
                    }
                },
            );
            assert_eq!(actual, Some(expected));
        }
        assert!(
            detect(1920, 1080, 24, |x, _| {
                if (800..1120).contains(&x) { 160 } else { 16 }
            })
            .is_none()
        );
    }

    #[test]
    fn seek_reset_discards_stale_answers_even_if_the_paused_target_is_dark() {
        let old = bars(0, 112, 720, 372);
        let mut crop = AutoCrop {
            enabled: true,
            ..Default::default()
        };
        crop.tracker.observe(Some(old), true);
        crop.last_pts = Some(10.0);
        crop.sampled_pts = Some((10.0, true));
        let old_generation = crop.generation;
        crop.reset(); // Explicit invalidation for the 10s -> 11s seek.
        assert!(crop.crop().is_none());
        assert!(crop.sampled_pts.is_none());
        for result in [Ok(Some(old)), Err(())] {
            crop.accept_answer(Answer {
                generation: old_generation,
                crop: result,
                paused: true,
            });
            assert!(crop.crop().is_none());
            assert!(!crop.unavailable);
        }
        crop.accept_answer(Answer {
            generation: crop.generation,
            crop: Ok(None),
            paused: true,
        });
        assert!(crop.crop().is_none());
        let new = bars(90, 0, 630, 480);
        crop.accept_answer(Answer {
            generation: crop.generation,
            crop: Ok(Some(new)),
            paused: true,
        });
        assert_eq!(crop.crop(), Some(new));
    }
    #[test]
    fn crop_waits_for_stability_ignores_fades_and_reveals_new_content() {
        let cropped = bars(0, 113, 720, 371);
        let full = bars(0, 0, 720, 480);
        let mut tracker = Tracker::default();
        for _ in 0..2 {
            tracker.observe(Some(cropped), false);
            assert!(tracker.crop.is_none());
        }
        tracker.observe(Some(cropped), false);
        assert_eq!(tracker.crop, Some(cropped));
        tracker.observe(None, false);
        assert_eq!(tracker.crop, Some(cropped));
        tracker.observe(Some(full), false);
        assert_eq!(tracker.crop, Some(full));
        tracker.observe(Some(cropped), false);
        assert_eq!(tracker.crop, Some(full));
        tracker.observe(Some(full), false);
        tracker.observe(Some(cropped), false);
        assert_eq!(tracker.crop, Some(full));
        tracker.observe(Some(cropped), true);
        assert_eq!(tracker.crop, Some(cropped));
    }
    #[test]
    fn luma_sampling_handles_depth_byte_order_and_negative_stride() {
        for (depth, shift, big_endian) in
            [(8, 0, 0), (10, 0, 0), (10, 6, 0), (12, 0, 1), (16, 0, 1)]
        {
            let bytes = if depth + shift <= 8 { 1 } else { 2 };
            let mut data = vec![0u8; 8 * bytes];
            for (i, luma) in [16u16, 40, 100, 240, 240, 100, 40, 16]
                .into_iter()
                .enumerate()
            {
                let encoded = luma << (depth - 8 + shift);
                if bytes == 1 {
                    data[i] = encoded as u8;
                } else {
                    let value = if big_endian != 0 {
                        encoded.to_be_bytes()
                    } else {
                        encoded.to_le_bytes()
                    };
                    data[i * 2..i * 2 + 2].copy_from_slice(&value);
                }
            }
            let view = Luma(ffi::UpLumaView {
                data: unsafe { data.as_ptr().add(4 * bytes) },
                width: 4,
                height: 2,
                stride: -(4 * bytes as i32),
                step: bytes as i32,
                depth,
                shift,
                big_endian,
                ..Default::default()
            });
            assert_eq!(view.sample(0, 0), 240);
            assert_eq!(view.sample(3, 0), 16);
            assert_eq!(view.sample(0, 1), 16);
            assert_eq!(view.sample(3, 1), 240);
        }
    }
    #[test]
    fn disabling_and_resetting_discard_detected_crop() {
        let mut crop = AutoCrop {
            enabled: true,
            ..Default::default()
        };
        crop.tracker.observe(Some(bars(0, 112, 720, 372)), true);
        assert!(crop.crop().is_some());
        let generation = crop.generation;
        assert!(!crop.toggle());
        assert!(crop.crop().is_none());
        assert_ne!(crop.generation, generation);
    }
    #[test]
    fn decoded_luma_detects_bars_in_eight_and_ten_bit_video() {
        use crate::decoder::Decoder;
        use std::{ffi::CString, process::Command, ptr};
        for format in ["yuv420p", "yuv420p10le"] {
            let path = std::env::temp_dir().join(format!(
                "undefined-player-crop-{}-{format}.mkv",
                std::process::id()
            ));
            let output = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=white:s=80x64:r=1",
                    "-vf",
                    "pad=128:96:24:16:black",
                    "-frames:v",
                    "1",
                    "-c:v",
                    "ffv1",
                    "-pix_fmt",
                    format,
                    "-threads",
                    "1",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            unsafe {
                let mut input = ptr::null_mut();
                let name = CString::new(path.to_str().unwrap()).unwrap();
                assert_eq!(ffi::up_av_format_open(&mut input, name.as_ptr()), 0);
                assert!(ffi::up_av_format_find_stream_info(input) >= 0);
                let stream =
                    ffi::up_av_find_best_stream(input, ffi::UpMediaType_UP_MEDIA_TYPE_VIDEO, -1);
                let decoder = Decoder::open(input, stream, None).unwrap();
                let mut packet = ffi::up_av_packet_alloc();
                assert!(ffi::up_av_read_frame(input, packet) >= 0);
                assert!(ffi::up_av_decoder_send_packet(decoder.as_ptr(), packet) >= 0);
                let mut raw = ptr::null_mut();
                assert!(ffi::up_av_decoder_receive_frame(decoder.as_ptr(), &mut raw) >= 0);
                let frame = VideoFrame::from_raw(raw, 0.0, 1.0);
                let reference = frame.clone_reference().unwrap();
                drop(frame);
                let luma = Luma::new(&reference).unwrap();
                assert_eq!(
                    luma.detect(),
                    Some(Crop {
                        width: 128,
                        height: 96,
                        left: 24,
                        top: 16,
                        right: 104,
                        bottom: 80
                    })
                );
                drop(luma);
                drop(reference);
                drop(decoder);
                ffi::up_av_packet_free(&mut packet);
                ffi::up_av_format_close(&mut input);
            }
            std::fs::remove_file(path).unwrap();
        }
    }
}
