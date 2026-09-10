use crate::ffi;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct Artwork {
    pub(crate) path: PathBuf,
}

impl Artwork {
    pub(crate) unsafe fn extract(format: *const ffi::UpAvFormat) -> Option<Self> {
        for index in 0..unsafe { ffi::up_av_stream_count(format) } {
            let mut data = std::ptr::null();
            let size = unsafe { ffi::up_av_stream_attached_picture(format, index, &mut data) };
            if data.is_null() || size == 0 || size > 16 * 1024 * 1024 {
                continue;
            }
            // Desktop widgets often force a square thumbnail. Pad the image
            // before exporting it so they cannot distort a portrait cover.
            let mut png_size = 0;
            let png = unsafe { ffi::up_av_artwork_png(format, index, &mut png_size) };
            if png.is_null() {
                continue;
            }
            let stored = Self::store(unsafe { std::slice::from_raw_parts(png, png_size) });
            unsafe { ffi::up_av_artwork_free(png) };
            match stored {
                Ok(artwork) => return Some(artwork),
                Err(error) => {
                    eprintln!("warning: could not prepare embedded artwork: {error}");
                    return None;
                }
            }
        }
        None
    }

    fn store(data: &[u8]) -> io::Result<Self> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "undefined-player-cover-{}-{serial}.png",
                std::process::id()
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    let artwork = Self { path };
                    file.write_all(data)?;
                    return Ok(artwork);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create artwork file",
        ))
    }
}

impl Drop for Artwork {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
