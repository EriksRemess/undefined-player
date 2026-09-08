use crate::decoder::ffmpeg_error;
use crate::media::AV_NOPTS_VALUE;
use crate::window::sdl_error;
use crate::{Result, ffi};
use std::collections::VecDeque;
use std::ptr;

pub(crate) const AUDIO_RATE: i32 = 48_000;
pub(crate) const AUDIO_CHANNELS: i32 = 2;
pub(crate) const AUDIO_BYTES_PER_FRAME: i64 = (size_of::<f32>() * AUDIO_CHANNELS as usize) as i64;
pub(crate) const AUDIO_QUEUE_TARGET_BYTES: i32 =
    AUDIO_RATE * AUDIO_BYTES_PER_FRAME as i32 * 150 / 1000;
// Stop demuxing independently of video, including after the last video packet.
pub(crate) const AUDIO_QUEUE_MAX_BYTES: i32 = AUDIO_RATE * AUDIO_BYTES_PER_FRAME as i32;
pub(crate) const AUDIO_TIMESTAMP_TOLERANCE: f64 = 0.002;
pub(crate) struct AudioChunk {
    pub(crate) pts: f64,
    pub(crate) samples: Vec<f32>,
    pub(crate) offset: usize,
}

pub(crate) struct AudioOutput {
    pub(crate) stream: *mut ffi::UpAudioStream,
    converter: *mut ffi::UpAvAudioConverter,
    pub(crate) first_pts: Option<f64>,
    pub(crate) submitted_frames: i64,
    segment_frames: i64,
    next_input_pts: Option<f64>,
    next_output_pts: f64,
    pub(crate) pending: VecDeque<AudioChunk>,
    pub(crate) resumed: bool,
}

impl AudioOutput {
    pub(crate) unsafe fn create() -> Result<Self> {
        let stream = unsafe { ffi::up_audio_stream_create(AUDIO_RATE, AUDIO_CHANNELS) };
        if stream.is_null() {
            return Err(format!("could not open PipeWire audio: {}", unsafe {
                sdl_error()
            }));
        }

        Ok(Self {
            stream,
            converter: ptr::null_mut(),
            first_pts: None,
            submitted_frames: 0,
            segment_frames: 0,
            next_input_pts: None,
            next_output_pts: 0.0,
            pending: VecDeque::new(),
            resumed: false,
        })
    }

    pub(crate) unsafe fn initialize_converter(
        &mut self,
        frame: *const ffi::UpAvFrame,
    ) -> Result<()> {
        if !self.converter.is_null() {
            return Ok(());
        }
        let mut error = 0;
        self.converter = unsafe {
            ffi::up_av_audio_converter_create(frame, AUDIO_RATE, AUDIO_CHANNELS, &mut error)
        };
        if self.converter.is_null() {
            return Err(format!(
                "could not configure audio conversion: {}",
                unsafe { ffmpeg_error(error) }
            ));
        }
        Ok(())
    }

    pub(crate) unsafe fn push(
        &mut self,
        frame: *const ffi::UpAvFrame,
        time_base: f64,
        discard_before: Option<f64>,
    ) -> Result<bool> {
        let timestamp = unsafe { ffi::up_av_frame_timestamp(frame) };
        let frame_pts = (timestamp != AV_NOPTS_VALUE)
            .then_some(timestamp as f64 * time_base)
            .filter(|pts| pts.is_finite());
        let discontinuity = self
            .next_input_pts
            .zip(frame_pts)
            .is_some_and(|(expected, actual)| {
                (actual - expected).abs() > AUDIO_TIMESTAMP_TOLERANCE
            });
        let changed = !self.converter.is_null()
            && unsafe { ffi::up_av_audio_converter_matches(self.converter, frame) } == 0;
        if discontinuity || changed {
            unsafe { self.drain_converter(discard_before)? };
            unsafe { ffi::up_av_audio_converter_free(&mut self.converter) };
        }
        if self.next_input_pts.is_none() || discontinuity {
            self.next_output_pts = frame_pts.unwrap_or(self.next_output_pts);
        }
        let input_pts = frame_pts
            .or(self.next_input_pts)
            .unwrap_or(self.next_output_pts);
        self.next_input_pts = Some(input_pts + unsafe { ffi::up_av_frame_audio_duration(frame) });
        unsafe { self.initialize_converter(frame)? };

        let capacity = unsafe { ffi::up_av_audio_converter_capacity(self.converter, frame) };
        if capacity < 0 {
            return Err(format!(
                "could not calculate converted audio size: {}",
                unsafe { ffmpeg_error(capacity) }
            ));
        }
        let mut samples = vec![0_f32; capacity as usize * AUDIO_CHANNELS as usize];
        let converted = unsafe {
            ffi::up_av_audio_converter_convert(
                self.converter,
                frame,
                samples.as_mut_ptr(),
                capacity,
            )
        };
        if converted < 0 {
            return Err(format!("audio conversion failed: {}", unsafe {
                ffmpeg_error(converted)
            }));
        }
        samples.truncate(converted as usize * AUDIO_CHANNELS as usize);
        let retained = self.append_samples(samples, discard_before);
        unsafe { self.pump()? };
        Ok(retained)
    }

    pub(crate) fn append_samples(
        &mut self,
        samples: Vec<f32>,
        discard_before: Option<f64>,
    ) -> bool {
        let frames = samples.len() / AUDIO_CHANNELS as usize;
        let pts = self.next_output_pts;
        self.next_output_pts += frames as f64 / AUDIO_RATE as f64;
        let skipped = discard_before
            .map_or(0, |target| {
                ((target - pts).max(0.0) * AUDIO_RATE as f64).ceil() as usize
            })
            .min(frames);
        if skipped == frames {
            return false;
        }
        self.pending.push_back(AudioChunk {
            pts,
            samples,
            offset: skipped * AUDIO_CHANNELS as usize,
        });
        true
    }

    pub(crate) unsafe fn drain_converter(&mut self, discard_before: Option<f64>) -> Result<()> {
        if self.converter.is_null() {
            return Ok(());
        }
        loop {
            let capacity = unsafe { ffi::up_av_audio_converter_drain_capacity(self.converter) };
            if capacity < 0 {
                return Err(format!("could not size buffered audio: {}", unsafe {
                    ffmpeg_error(capacity)
                }));
            }
            if capacity == 0 {
                break;
            }
            let mut samples = vec![0_f32; capacity as usize * AUDIO_CHANNELS as usize];
            let converted = unsafe {
                ffi::up_av_audio_converter_drain(self.converter, samples.as_mut_ptr(), capacity)
            };
            if converted < 0 {
                return Err(format!("could not drain audio conversion: {}", unsafe {
                    ffmpeg_error(converted)
                }));
            }
            if converted == 0 {
                break;
            }
            samples.truncate(converted as usize * AUDIO_CHANNELS as usize);
            self.append_samples(samples, discard_before);
        }
        unsafe { self.pump() }
    }

    pub(crate) unsafe fn pump(&mut self) -> Result<()> {
        while let Some(chunk) = self.pending.front() {
            let pts =
                chunk.pts + (chunk.offset / AUDIO_CHANNELS as usize) as f64 / AUDIO_RATE as f64;
            let queued = unsafe { self.queued_bytes() };
            let end = self
                .first_pts
                .map(|start| start + self.segment_frames as f64 / AUDIO_RATE as f64);
            if end.is_none_or(|end| (pts - end).abs() > AUDIO_TIMESTAMP_TOLERANCE) {
                if queued != 0 {
                    break;
                }
                // Never concatenate separate timestamp ranges in SDL's un-timed
                // stream. The event loop resumes this segment when it is due.
                unsafe { self.set_paused(true)? };
                self.first_pts = Some(pts);
                self.segment_frames = 0;
            }
            let available = (AUDIO_QUEUE_MAX_BYTES - queued).max(0) as usize / size_of::<f32>();
            let chunk = self.pending.front_mut().unwrap();
            let count = (chunk.samples.len() - chunk.offset).min(available);
            if count == 0 {
                break;
            }
            if unsafe {
                ffi::up_audio_stream_put(
                    self.stream,
                    chunk.samples.as_ptr().add(chunk.offset).cast(),
                    (count * size_of::<f32>()) as i32,
                )
            } == 0
            {
                return Err(format!("could not queue audio: {}", unsafe { sdl_error() }));
            }
            chunk.offset += count;
            let frames = (count / AUDIO_CHANNELS as usize) as i64;
            self.submitted_frames += frames;
            self.segment_frames += frames;
            if chunk.offset == chunk.samples.len() {
                self.pending.pop_front();
            }
        }
        Ok(())
    }

    pub(crate) unsafe fn full(&self) -> bool {
        !self.pending.is_empty() || unsafe { self.queued_bytes() } >= AUDIO_QUEUE_MAX_BYTES
    }

    pub(crate) unsafe fn active_clock(&self) -> Option<f64> {
        if self.resumed && unsafe { self.queued_bytes() } > 0 {
            unsafe { self.clock() }
        } else {
            None
        }
    }

    pub(crate) unsafe fn queued_bytes(&self) -> i32 {
        unsafe { ffi::up_audio_stream_queued(self.stream) }.max(0)
    }

    pub(crate) unsafe fn clock(&self) -> Option<f64> {
        let base = self.first_pts?;
        let queued_frames = unsafe { self.queued_bytes() } as i64 / AUDIO_BYTES_PER_FRAME;
        Some(base + (self.segment_frames - queued_frames) as f64 / AUDIO_RATE as f64)
    }

    pub(crate) unsafe fn set_paused(&mut self, paused: bool) -> Result<()> {
        if self.resumed == !paused {
            return Ok(());
        }
        let ok = if paused {
            unsafe { ffi::up_audio_stream_pause(self.stream) }
        } else {
            unsafe { ffi::up_audio_stream_resume(self.stream) }
        };
        if ok == 0 {
            return Err(format!("could not change audio pause state: {}", unsafe {
                sdl_error()
            }));
        }
        self.resumed = !paused;
        Ok(())
    }

    pub(crate) unsafe fn reset(&mut self) -> Result<()> {
        if unsafe { ffi::up_audio_stream_clear(self.stream) } == 0 {
            return Err(format!("could not clear queued audio: {}", unsafe {
                sdl_error()
            }));
        }
        unsafe { ffi::up_av_audio_converter_free(&mut self.converter) };
        self.first_pts = None;
        self.submitted_frames = 0;
        self.segment_frames = 0;
        self.next_input_pts = None;
        self.next_output_pts = 0.0;
        self.pending.clear();
        Ok(())
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        unsafe {
            ffi::up_av_audio_converter_free(&mut self.converter);
            ffi::up_audio_stream_destroy(self.stream);
        }
    }
}
