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
#[cfg(target_os = "macos")]
mod macos_app;
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
    let arguments: Vec<_> = arguments.collect();
    #[cfg(target_os = "macos")]
    let gui_launch = macos_app::is_bundle() && arguments.is_empty();
    #[cfg(not(target_os = "macos"))]
    let gui_launch = false;
    let action = match if gui_launch {
        Ok(None)
    } else {
        parse_cli(arguments).map(Some)
    } {
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
        Some(CliAction::Help) => {
            println!("{}", usage(Path::new(&program)));
            return;
        }
        Some(CliAction::Version) => {
            println!("undefined-player {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some(CliAction::Play {
            source,
            perf_log,
            debug,
        }) => (Some(source), perf_log, debug),
        None => (None, false, false),
    };
    unsafe { ffi::up_av_set_debug(i32::from(debug)) };
    if let Some(source) = &source
        && source.local_path().is_some_and(|path| !path.is_file())
    {
        eprintln!(
            "undefined-player: {} is not a file",
            source.display().to_string_lossy()
        );
        std::process::exit(2);
    }

    if let Err(error) = run_player(source, perf_log) {
        eprintln!("undefined-player: {error}");
        #[cfg(target_os = "macos")]
        if macos_app::is_bundle() {
            macos_app::show_error(&error);
        }
        std::process::exit(1);
    }
}

fn run_player(source: Option<source::MediaSource>, perf_log: bool) -> Result<()> {
    let _sdl = unsafe { window::Sdl::init()? };
    #[cfg(target_os = "macos")]
    let source = match source {
        Some(source) => Some(source),
        None => macos_app::choose_source()?,
    };
    let mut next = source;
    while let Some(source) = next {
        let source = youtube::resolve(source)?;
        let is_youtube = matches!(source, source::MediaSource::YouTube { .. });
        next = unsafe { playback::run(source, perf_log) }.map_err(|error| {
            if is_youtube {
                youtube::playback_error(&error)
            } else {
                error
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
