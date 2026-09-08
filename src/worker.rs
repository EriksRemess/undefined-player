use crate::Result;
use crate::decoder::VideoFrame;
use crate::media::{Media, VIDEO_QUEUE_MAX};
use crate::subtitles::SubtitleCue;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) struct DecodeWorker {
    media: Arc<Mutex<Media>>,
    running: Arc<AtomicBool>,
    fill_nanoseconds: Arc<AtomicU64>,
    outstanding_frames: Arc<AtomicUsize>,
    frames: mpsc::Receiver<QueuedVideoFrame>,
    errors: mpsc::Receiver<String>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) struct QueuedVideoFrame {
    pub(crate) frame: VideoFrame,
    outstanding_frames: Arc<AtomicUsize>,
}

impl Drop for QueuedVideoFrame {
    fn drop(&mut self) {
        self.outstanding_frames.fetch_sub(1, Ordering::Release);
    }
}

impl DecodeWorker {
    pub(crate) fn pending_frames(&self) -> usize {
        self.outstanding_frames.load(Ordering::Acquire)
    }

    pub(crate) fn start(media: Media, path: PathBuf, measure_performance: bool) -> Self {
        let media = Arc::new(Mutex::new(media));
        let running = Arc::new(AtomicBool::new(true));
        let fill_nanoseconds = Arc::new(AtomicU64::new(0));
        let outstanding_frames = Arc::new(AtomicUsize::new(0));
        let (frame_sender, frames) = mpsc::channel();
        let (error_sender, errors) = mpsc::channel();
        let thread_media = Arc::clone(&media);
        let thread_running = Arc::clone(&running);
        let thread_fill_nanoseconds = Arc::clone(&fill_nanoseconds);
        let thread_outstanding_frames = Arc::clone(&outstanding_frames);
        let thread = thread::spawn(move || {
            while thread_running.load(Ordering::Acquire) {
                let started = Instant::now();
                let result = match thread_media.lock() {
                    Ok(mut media) => {
                        let mut result = unsafe { media.fill_queues() };
                        if let Err(error) = &result
                            && media.video.uses_vulkan
                            && media.first_video_pts.is_none()
                        {
                            // Audio can fill the startup buffer before video is
                            // decoded. Preserve initial hardware fallback here too.
                            eprintln!(
                                "warning: Vulkan decoding failed ({error}); restarting with software decoding"
                            );
                            let target = unsafe { media.audio_clock() }.unwrap_or(0.0);
                            let selected_audio_track = media.selected_audio_track;
                            result = unsafe { Media::open(&path, None, measure_performance) }
                                .and_then(|mut replacement| {
                                    replacement.selected_audio_track = selected_audio_track;
                                    replacement.next_video_pts = target;
                                    replacement.video_seek_target = Some(target);
                                    replacement.audio_seek_target = Some(target);
                                    replacement.subtitle_seek_target = Some(target);
                                    *media = replacement;
                                    unsafe { media.fill_queues() }
                                });
                        }
                        while result.is_ok()
                            && thread_outstanding_frames.load(Ordering::Acquire) < VIDEO_QUEUE_MAX
                        {
                            let Some(frame) = media.video_queue.pop_front() else {
                                break;
                            };
                            thread_outstanding_frames.fetch_add(1, Ordering::Release);
                            if frame_sender
                                .send(QueuedVideoFrame {
                                    frame,
                                    outstanding_frames: Arc::clone(&thread_outstanding_frames),
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        result
                    }
                    Err(_) => Err("decoder state lock was poisoned".into()),
                };
                if measure_performance {
                    let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    thread_fill_nanoseconds.fetch_add(elapsed, Ordering::Relaxed);
                }
                if let Err(error) = result {
                    let _ = error_sender.send(error);
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            media,
            running,
            fill_nanoseconds,
            outstanding_frames,
            frames,
            errors,
            thread: Some(thread),
        }
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Media>> {
        self.media
            .lock()
            .map_err(|_| "decoder state lock was poisoned".into())
    }

    pub(crate) fn try_lock(&self) -> Result<Option<MutexGuard<'_, Media>>> {
        match self.media.try_lock() {
            Ok(media) => Ok(Some(media)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Poisoned(_)) => Err("decoder state lock was poisoned".into()),
        }
    }

    pub(crate) fn check_error(&self) -> Result<()> {
        match self.errors.try_recv() {
            Ok(error) => Err(error),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => Ok(()),
        }
    }

    pub(crate) fn receive_frames(&self, queue: &mut VecDeque<QueuedVideoFrame>) {
        while queue.len() < VIDEO_QUEUE_MAX {
            match self.frames.try_recv() {
                Ok(frame) => queue.push_back(frame),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    pub(crate) fn receive_subtitles(&self, queue: &mut VecDeque<SubtitleCue>) -> Result<()> {
        if let Some(mut media) = self.try_lock()? {
            queue.append(&mut media.subtitle_queue);
        }
        Ok(())
    }

    pub(crate) fn clear_frames(
        &self,
        queue: &mut VecDeque<QueuedVideoFrame>,
        current: &mut Option<QueuedVideoFrame>,
    ) {
        queue.clear();
        *current = None;
        while self.frames.try_recv().is_ok() {}
    }

    pub(crate) fn clear_subtitles(
        &self,
        incoming: &mut VecDeque<SubtitleCue>,
        queues: &mut [VecDeque<SubtitleCue>],
        current: &mut [Option<SubtitleCue>],
    ) {
        incoming.clear();
        for queue in queues {
            queue.clear();
        }
        for subtitle in current {
            *subtitle = None;
        }
    }

    pub(crate) fn take_fill_time(&self) -> Duration {
        Duration::from_nanos(self.fill_nanoseconds.swap(0, Ordering::Relaxed))
    }
}

impl Drop for DecodeWorker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        while self.frames.try_recv().is_ok() {}
        debug_assert_eq!(self.outstanding_frames.load(Ordering::Acquire), 0);
    }
}
