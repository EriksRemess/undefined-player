#![allow(clippy::missing_safety_doc)]

use cli::{CliAction, parse_cli, usage};
use std::env;
use std::path::{Path, PathBuf};

mod audio;
mod autocrop;
mod cli;
mod clock;
mod decoder;
mod deinterlace;
#[allow(warnings, clippy::all)]
mod ffi;
mod geometry;
mod media;
mod metadata;
mod mpris;
mod overlay;
mod pixel_font;
mod playback;
mod presentation;
mod renderer;
mod subtitles;
mod window;
mod worker;
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
    let (path, perf_log) = match action {
        CliAction::Help => {
            println!("{}", usage(Path::new(&program)));
            return;
        }
        CliAction::Version => {
            println!("undefined-player {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        CliAction::Play { path, perf_log } => (path, perf_log),
    };
    if !path.is_file() {
        eprintln!("undefined-player: {} is not a file", path.display());
        std::process::exit(2);
    }

    if let Err(error) = unsafe { playback::run(path, perf_log) } {
        eprintln!("undefined-player: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
