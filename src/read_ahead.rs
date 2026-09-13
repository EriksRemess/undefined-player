use std::{
    collections::VecDeque,
    ffi::{CStr, OsStr, c_char, c_int, c_void},
    fs::File,
    io,
    os::unix::{ffi::OsStrExt, fs::FileExt},
    ptr, slice,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

const AHEAD: usize = 4 * 1024 * 1024;
const BACK: usize = 4 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
// The native adapter uses FFmpeg's negative errno convention (Linux only).
const INVALID: i32 = -22;

fn error_code(error: io::Error) -> i32 {
    -error.raw_os_error().unwrap_or(5)
}

struct State {
    bytes: VecDeque<u8>,
    start: i64,
    position: i64,
    generation: u64,
    error: Option<i32>, // Zero means EOF, negative values are errno.
    stopped: bool,
}

impl State {
    fn end(&self) -> i64 {
        self.start + self.bytes.len() as i64
    }

    fn trim(&mut self) {
        let discard = (self.position - self.start).saturating_sub(BACK as i64);
        if discard > 0 {
            self.bytes.drain(..discard as usize);
            self.start += discard;
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct ReadAhead {
    shared: Arc<Shared>,
    size: i64,
    worker: Option<JoinHandle<()>>,
}

impl ReadAhead {
    fn open(path: &OsStr) -> io::Result<Self> {
        let file = File::open(path)?;
        let size =
            i64::try_from(file.metadata()?.len()).map_err(|_| io::Error::from_raw_os_error(75))?;
        Self::new(size, move |buffer, offset| file.read_at(buffer, offset))
    }

    fn new(
        size: i64,
        mut read_at: impl FnMut(&mut [u8], u64) -> io::Result<usize> + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                bytes: VecDeque::with_capacity(AHEAD + BACK),
                start: 0,
                position: 0,
                generation: 0,
                error: None,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let producer = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("file-read".into())
            .spawn(move || {
                let mut buffer = [0; CHUNK];
                let mut state = producer.state.lock().unwrap();
                loop {
                    if state.stopped {
                        break;
                    }
                    let ahead = (state.end() - state.position) as usize;
                    if state.error.is_some() || ahead >= AHEAD {
                        state = producer.changed.wait(state).unwrap();
                        continue;
                    }
                    let count = CHUNK
                        .min(AHEAD - ahead)
                        .min(AHEAD + BACK - state.bytes.len())
                        .min((i64::MAX - state.end()) as usize);
                    let offset = state.end();
                    let generation = state.generation;
                    // Never hold the buffer lock across a potentially slow
                    // network read. Positional reads also avoid a shared cursor.
                    drop(state);
                    let result = loop {
                        match read_at(&mut buffer[..count], offset as u64) {
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            result => break result,
                        }
                    };
                    state = producer.state.lock().unwrap();
                    // A seek can replace the buffer while this read is in flight.
                    // Discard both bytes and errors belonging to the old position.
                    if generation != state.generation {
                        continue;
                    }
                    match result {
                        Ok(0) => state.error = Some(0),
                        Ok(count) => state.bytes.extend(&buffer[..count]),
                        Err(error) => state.error = Some(error_code(error)),
                    }
                    producer.changed.notify_all();
                }
            })?;
        Ok(Self {
            shared,
            size,
            worker: Some(worker),
        })
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, i32> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.state.lock().unwrap();
        while state.position == state.end() && state.error.is_none() {
            state = self.shared.changed.wait(state).unwrap();
        }
        let count = output.len().min((state.end() - state.position) as usize);
        if count == 0 {
            return match state.error.unwrap() {
                0 => Ok(0),
                error => Err(error),
            };
        }
        let history = (state.position - state.start) as usize;
        let (first, second) = state.bytes.as_slices();
        let first_count = count.min(first.len().saturating_sub(history));
        if first_count > 0 {
            output[..first_count].copy_from_slice(&first[history..history + first_count]);
        }
        let second_start = history.saturating_sub(first.len());
        output[first_count..count]
            .copy_from_slice(&second[second_start..second_start + count - first_count]);
        state.position += count as i64;
        state.trim();
        self.shared.changed.notify_all();
        Ok(count)
    }

    fn seek(&self, offset: i64, whence: i32) -> Result<i64, i32> {
        let mut state = self.shared.state.lock().unwrap();
        let base = match whence {
            0 => 0,              // SEEK_SET
            1 => state.position, // SEEK_CUR
            2 => self.size,      // SEEK_END
            _ => return Err(INVALID),
        };
        let target = base
            .checked_add(offset)
            .filter(|n| *n >= 0)
            .ok_or(INVALID)?;
        if target < state.start || target > state.end() {
            state.bytes.clear();
            state.start = target;
            state.error = None;
            state.generation = state.generation.wrapping_add(1);
        }
        state.position = target;
        state.trim();
        self.shared.changed.notify_all();
        Ok(target)
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stopped = true;
        self.shared.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Only the C AVIO adapter owns these opaque pointers. FFmpeg calls read/seek
// serially on the demux worker, then closes the reader before freeing its AVIO.
#[unsafe(no_mangle)]
unsafe extern "C" fn up_read_ahead_open(path: *const c_char, error: *mut c_int) -> *mut c_void {
    let path = OsStr::from_bytes(unsafe { CStr::from_ptr(path) }.to_bytes());
    match ReadAhead::open(path) {
        Ok(reader) => Box::into_raw(Box::new(reader)).cast(),
        Err(failure) => {
            unsafe { *error = error_code(failure) };
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn up_read_ahead_read(
    reader: *mut c_void,
    buffer: *mut u8,
    size: c_int,
) -> c_int {
    if size <= 0 {
        return INVALID;
    }
    let reader = unsafe { &*reader.cast::<ReadAhead>() };
    let buffer = unsafe { slice::from_raw_parts_mut(buffer, size as usize) };
    reader
        .read(buffer)
        .map_or_else(|error| error, |count| count as c_int)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn up_read_ahead_seek(reader: *mut c_void, offset: i64, whence: c_int) -> i64 {
    unsafe { &*reader.cast::<ReadAhead>() }
        .seek(offset, whence)
        .unwrap_or_else(i64::from)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn up_read_ahead_size(reader: *mut c_void) -> i64 {
    unsafe { &*reader.cast::<ReadAhead>() }.size
}

#[unsafe(no_mangle)]
unsafe extern "C" fn up_read_ahead_close(reader: *mut c_void) {
    drop(unsafe { Box::from_raw(reader.cast::<ReadAhead>()) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    fn byte(offset: u64) -> u8 {
        (offset.wrapping_mul(31) ^ (offset >> 8) ^ (offset >> 17)) as u8
    }

    fn fill(buffer: &mut [u8], offset: u64, size: u64) -> io::Result<usize> {
        let count = buffer.len().min(size.saturating_sub(offset) as usize);
        for (index, value) in buffer[..count].iter_mut().enumerate() {
            *value = byte(offset + index as u64);
        }
        Ok(count)
    }

    fn check(reader: &ReadAhead, offset: i64, count: usize) {
        let mut buffer = vec![0; count];
        let mut read = 0;
        while read < count {
            let n = reader.read(&mut buffer[read..]).unwrap();
            assert!(n > 0);
            read += n;
        }
        for (index, value) in buffer.into_iter().enumerate() {
            assert_eq!(value, byte(offset as u64 + index as u64), "at {index}");
        }
        let state = reader.shared.state.lock().unwrap();
        assert!(state.bytes.len() <= AHEAD + BACK);
        assert!(state.position - state.start <= BACK as i64);
    }

    #[test]
    fn wrapped_buffer_random_seeks_and_eof_preserve_every_byte() {
        let size = 24 * 1024 * 1024;
        let reader = ReadAhead::new(size, move |b, p| fill(b, p, size as u64)).unwrap();
        check(&reader, 0, size as usize);
        assert_eq!(reader.read(&mut [0; 1]), Ok(0));
        let mut random = 23_u64;
        for _ in 0..200 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let position = (random % (size as u64 - 100_000)) as i64;
            assert_eq!(reader.seek(position, 0), Ok(position));
            check(&reader, position, 100_000);
            assert_eq!(reader.seek(-500, 1), Ok(position + 99_500));
            check(&reader, position + 99_500, 1000);
        }
        assert_eq!(reader.seek(-100, 2), Ok(size - 100));
        check(&reader, size - 100, 100);
        assert_eq!(reader.read(&mut [0; 1]), Ok(0));
        assert_eq!(reader.seek(-1, 0), Err(INVALID));
        assert_eq!(reader.seek(i64::MAX, 1), Err(INVALID));
        assert_eq!(reader.seek(0, 7), Err(INVALID));
        assert_eq!(reader.seek(0, 0), Ok(0));
        check(&reader, 0, CHUNK);
    }

    #[test]
    fn stalled_reads_allow_consumption_and_seeks_discard_old_results() {
        for fail in [false, true] {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut reads = 0;
            let reader = ReadAhead::new(32 * AHEAD as i64, move |buffer, offset| {
                reads += 1;
                if reads == 2 {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if fail {
                        return Err(io::Error::from_raw_os_error(5));
                    }
                }
                fill(buffer, offset, 32 * AHEAD as u64)
            })
            .unwrap();
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            // The producer is blocked, but the first chunk remains readable.
            check(&reader, 0, CHUNK);
            let target = 16 * AHEAD as i64;
            assert_eq!(reader.seek(target, 0), Ok(target));
            release_tx.send(()).unwrap();
            check(&reader, target, 3 * CHUNK);
        }
    }

    #[test]
    fn buffered_bytes_precede_io_errors_and_seek_can_recover() {
        let reader = ReadAhead::new(8 * AHEAD as i64, |buffer, offset| {
            if offset == CHUNK as u64 {
                Err(io::Error::from_raw_os_error(5))
            } else {
                fill(buffer, offset, 8 * AHEAD as u64)
            }
        })
        .unwrap();
        check(&reader, 0, CHUNK);
        assert_eq!(reader.read(&mut [0; 1]), Err(-5));
        assert_eq!(reader.seek(4 * CHUNK as i64, 0), Ok(4 * CHUNK as i64));
        check(&reader, 4 * CHUNK as i64, CHUNK);
    }
}
