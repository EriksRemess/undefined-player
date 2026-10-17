#![allow(clippy::missing_safety_doc)]

use cli::{CliAction, parse_cli, usage};
use std::env;
use std::path::{Path, PathBuf};

mod artwork;
mod audio;
mod autocrop;
mod chapters;
mod cli;
mod clock;
mod config;
mod decoder;
mod deinterlace;
#[allow(warnings, clippy::all)]
mod ffi;
mod geometry;
mod hls;
mod language;
mod luma;
mod media;
mod metadata;
#[cfg_attr(target_os = "macos", path = "mpris_macos.rs")]
mod mpris;
mod overlay;
mod pixel_font;
mod playback;
mod presentation;
mod read_ahead;
mod renderer;
mod resume;
mod source;
mod subtitles;
mod window;
mod worker;
mod youtube;
mod zoom;

pub(crate) type Result<T> = std::result::Result<T, String>;

fn main() {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .and_then(|path| PathBuf::from(path).file_name().map(|name| name.to_owned()))
        .unwrap_or_else(|| "undefined-player".into());
    let action = match parse_cli(arguments) {
        Ok(action) => action,
        Err(error) => {
            eprintln!(
                "undefined-player: {error}\n\n{}",
                usage(Path::new(&program))
            );
            std::process::exit(2);
        }
    };
    let (source, perf_log, debug) = match action {
        CliAction::Help => {
            println!("{}", usage(Path::new(&program)));
            return;
        }
        CliAction::Version => {
            println!("undefined-player {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        CliAction::Play {
            source,
            perf_log,
            debug,
        } => (source, perf_log, debug),
    };
    unsafe { ffi::up_av_set_debug(i32::from(debug)) };
    if source.local_path().is_some_and(|path| !path.is_file()) {
        eprintln!(
            "undefined-player: {} is not a file",
            source.display().to_string_lossy()
        );
        std::process::exit(2);
    }

    let source = match youtube::resolve(source) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("undefined-player: {error}");
            std::process::exit(1);
        }
    };
    let is_youtube = matches!(source, source::MediaSource::YouTube { .. });
    if let Err(error) = unsafe { playback::run(source, perf_log) } {
        let error = if is_youtube {
            youtube::playback_error(&error)
        } else {
            error
        };
        eprintln!("undefined-player: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
