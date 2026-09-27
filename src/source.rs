use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MediaSource {
    File(PathBuf),
    Http(String),
}

pub(crate) trait MediaInput {
    fn input_bytes(&self) -> &[u8];
    fn is_network_input(&self) -> bool;
}

impl MediaSource {
    pub(crate) fn from_argument(argument: OsString) -> Self {
        let is_http = argument.to_str().is_some_and(|value| {
            value
                .get(..7)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
                || value
                    .get(..8)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        });
        if is_http {
            return Self::Http(argument.into_string().expect("HTTP URL is UTF-8"));
        }
        Self::File(PathBuf::from(argument))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        match self {
            Self::File(path) => path.as_os_str().as_bytes(),
            Self::Http(url) => url.as_bytes(),
        }
    }

    pub(crate) fn is_network(&self) -> bool {
        matches!(self, Self::Http(_))
    }

    pub(crate) fn local_path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Http(_) => None,
        }
    }

    pub(crate) fn display(&self) -> &OsStr {
        match self {
            Self::File(path) => path.as_os_str(),
            Self::Http(url) => OsStr::new(url),
        }
    }
}

impl MediaInput for MediaSource {
    fn input_bytes(&self) -> &[u8] {
        self.as_bytes()
    }

    fn is_network_input(&self) -> bool {
        self.is_network()
    }
}

impl MediaInput for Path {
    fn input_bytes(&self) -> &[u8] {
        self.as_os_str().as_bytes()
    }

    fn is_network_input(&self) -> bool {
        false
    }
}

impl MediaInput for PathBuf {
    fn input_bytes(&self) -> &[u8] {
        self.as_os_str().as_bytes()
    }

    fn is_network_input(&self) -> bool {
        false
    }
}
