use crate::Result;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CliAction {
    Help,
    Version,
    Play { path: PathBuf, perf_log: bool },
}

pub(crate) fn parse_cli(arguments: impl IntoIterator<Item = OsString>) -> Result<CliAction> {
    let mut path = None;
    let mut perf_log = false;
    let mut parse_options = true;
    for argument in arguments {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--perf" {
            perf_log = true;
        } else if parse_options && (argument == "-h" || argument == "--help") {
            return Ok(CliAction::Help);
        } else if parse_options && (argument == "-V" || argument == "--version") {
            return Ok(CliAction::Version);
        } else if parse_options && argument.as_encoded_bytes().starts_with(b"-") {
            return Err(format!("unknown option: {}", argument.to_string_lossy()));
        } else if path.is_none() {
            path = Some(PathBuf::from(argument));
        } else {
            return Err("only one media file can be played at a time".into());
        }
    }
    path.map_or_else(
        || Err("no media file was specified".into()),
        |path| Ok(CliAction::Play { path, perf_log }),
    )
}

pub(crate) fn usage(program: &Path) -> String {
    format!(
        "Usage: {} [OPTIONS] VIDEO\n\nOptions:\n  --perf         print playback performance statistics\n  -h, --help     show this help\n  -V, --version  show the version",
        program.display()
    )
}
