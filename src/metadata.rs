use crate::decoder::{ffmpeg_name, stream_metadata};
use crate::ffi;
use crate::media::Media;
use std::ffi::{CStr, CString};
use std::path::Path;

pub(crate) fn single_line_metadata(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

pub(crate) struct MediaMetadata {
    title: Option<String>,
    artist: Option<String>,
}

fn present_artist(artist: Option<String>) -> Option<String> {
    artist.filter(|artist| !artist.eq_ignore_ascii_case("unknown"))
}

impl MediaMetadata {
    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
    pub(crate) fn artist(&self) -> Option<&str> {
        self.artist.as_deref()
    }
    pub(crate) unsafe fn inspect(format: *const ffi::UpAvFormat) -> Self {
        let read = |key: &CStr| {
            let pointer = unsafe { ffi::up_av_format_metadata(format, key.as_ptr()) };
            if pointer.is_null() {
                return None;
            }
            let text = single_line_metadata(&unsafe { CStr::from_ptr(pointer) }.to_string_lossy());
            (!text.is_empty()).then_some(text)
        };
        Self {
            title: read(c"title"),
            artist: present_artist(read(c"artist")),
        }
    }

    pub(crate) fn overlay_text(&self, chapter: Option<&CStr>) -> CString {
        let mut lines = Vec::new();
        if let Some(title) = &self.title {
            lines.push(format!("TITLE: {}", title.to_uppercase()));
        }
        if let Some(artist) = &self.artist {
            lines.push(format!("ARTIST: {}", artist.to_uppercase()));
        }
        if let Some(chapter) = chapter {
            lines.push(chapter.to_string_lossy().into_owned());
        }
        CString::new(lines.join("\n")).expect("metadata has no NUL bytes")
    }
}

pub(crate) fn format_bitrate(bits_per_second: i64) -> String {
    if bits_per_second >= 1_000_000 {
        format!("{:.1} MBPS", bits_per_second as f64 / 1_000_000.0)
    } else if bits_per_second >= 1_000 {
        format!("{:.0} KBPS", bits_per_second as f64 / 1_000.0)
    } else if bits_per_second > 0 {
        format!("{bits_per_second} BPS")
    } else {
        "UNKNOWN".to_owned()
    }
}

pub(crate) fn format_video_bitrate(declared: i64, metadata: Option<i64>, container: i64) -> String {
    if declared > 0 {
        format_bitrate(declared)
    } else if let Some(metadata) = metadata.filter(|value| *value > 0) {
        format_bitrate(metadata)
    } else if container > 0 {
        format!("{} (CONTAINER)", format_bitrate(container))
    } else {
        "UNKNOWN".to_owned()
    }
}

pub(crate) fn mark_assumed(name: Option<String>, assumed: bool) -> String {
    let name = name.unwrap_or_else(|| "UNKNOWN".to_owned());
    if assumed {
        format!("{name} (ASSUMED)")
    } else {
        name
    }
}

pub(crate) fn hdr_status(kind: ffi::UpHdrKind, assumed: bool) -> &'static str {
    match kind {
        ffi::UpHdrKind_UP_HDR_KIND_PQ => "YES (PQ)",
        ffi::UpHdrKind_UP_HDR_KIND_HLG => "YES (HLG)",
        ffi::UpHdrKind_UP_HDR_KIND_UNKNOWN => "UNKNOWN",
        ffi::UpHdrKind_UP_HDR_KIND_SDR if assumed => "NO (ASSUMED)",
        _ => "NO",
    }
}

#[derive(Clone)]
pub(crate) struct TrackLabel {
    pub(crate) codec: String,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
}

impl TrackLabel {
    pub(crate) unsafe fn inspect(format: *const ffi::UpAvFormat, stream_index: u32) -> Self {
        let codec = unsafe { ffmpeg_name(ffi::up_av_stream_codec_name(format, stream_index)) }
            .unwrap_or_else(|| "UNKNOWN".to_owned());
        Self {
            codec,
            language: unsafe { stream_metadata(format, stream_index, c"language") },
            title: unsafe { stream_metadata(format, stream_index, c"title") },
        }
    }
}

pub(crate) struct VideoInfo {
    pub(crate) lines: [String; 10],
    pub(crate) frame_rate: Option<f64>,
}

impl VideoInfo {
    pub(crate) unsafe fn inspect(media: &Media, frame: *const ffi::UpAvFrame) -> Self {
        let mut info: ffi::UpVideoInfo = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { ffi::up_av_video_info(media.format, media.video.as_ptr(), frame, &mut info) },
            0,
            "opened video has inspectable stream information"
        );
        let codec = unsafe { ffmpeg_name(info.codec) }.unwrap_or_else(|| "UNKNOWN".to_owned());
        let profile = unsafe { ffmpeg_name(info.profile) };
        let codec = profile.map_or(codec.clone(), |profile| format!("{codec} {profile}"));

        let frame_rate =
            (info.frame_rate.is_finite() && info.frame_rate > 0.0).then_some(info.frame_rate);
        let bit_rate = format_video_bitrate(
            info.declared_bitrate,
            (info.metadata_bitrate > 0).then_some(info.metadata_bitrate),
            info.container_bitrate,
        );
        let pixel_format = unsafe { ffmpeg_name(info.pixel_format) }
            .unwrap_or_else(|| "UNKNOWN PIXEL FORMAT".to_owned());
        let decode_path = if media.video.uses_vulkan {
            "VULKAN HW"
        } else {
            "SOFTWARE"
        };
        let resolution_line = format!("RESOLUTION: {}X{}", info.width, info.height);
        let color_space = mark_assumed(
            unsafe { ffmpeg_name(info.color_space) },
            info.color_space_assumed != 0,
        );
        let color_primaries = mark_assumed(
            unsafe { ffmpeg_name(info.color_primaries) },
            info.color_primaries_assumed != 0,
        );
        let color_transfer = mark_assumed(
            unsafe { ffmpeg_name(info.color_transfer) },
            info.color_transfer_assumed != 0,
        );
        let color_range = mark_assumed(
            unsafe { ffmpeg_name(info.color_range) },
            info.color_range_assumed != 0,
        );
        let hdr = hdr_status(info.hdr_kind, info.color_transfer_assumed != 0);
        Self {
            lines: [
                format!("CODEC: {codec}"),
                resolution_line,
                format!("BITRATE: {bit_rate}"),
                format!("PIXEL FORMAT: {pixel_format}"),
                format!("DECODE: {decode_path}"),
                format!("MATRIX: {color_space}"),
                format!("PRIMARIES: {color_primaries}"),
                format!("TRANSFER: {color_transfer}"),
                format!("RANGE: {color_range}"),
                format!("HDR: {hdr}"),
            ],
            frame_rate,
        }
    }

    pub(crate) fn overlay_text(&self, audio: Option<(usize, usize, &TrackLabel)>) -> CString {
        let mut text = self.lines.join("\n");
        if let Some((selected, count, label)) = audio {
            text.push_str(&format!(
                "\nAUDIO TRACK: {} / {count}\nAUDIO CODEC: {}",
                selected + 1,
                label.codec
            ));
            if let Some(language) = label.language.as_deref() {
                text.push_str("\nAUDIO LANGUAGE: ");
                text.push_str(&language.to_uppercase());
            }
            if let Some(title) = label.title.as_deref() {
                text.push_str("\nAUDIO TITLE: ");
                text.push_str(&title.to_uppercase());
            }
        } else {
            text.push_str("\nAUDIO: NONE");
        }
        CString::new(text).expect("media information has no NUL bytes")
    }
}

pub(crate) fn media_title(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("undefined-player");
    let stem = stem
        .strip_suffix(']')
        .and_then(|value| value.rsplit_once(" ["))
        .filter(|(_, id)| {
            id.len() == 11
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .map_or(stem, |(title, _)| title);
    let title = stem.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "undefined-player".into()
    } else {
        title
    }
}

pub(crate) fn display_title(path: &Path) -> String {
    media_title(path).to_uppercase()
}

#[cfg(test)]
mod media_metadata_tests {
    use super::*;
    #[test]
    fn unknown_artist_is_omitted_from_all_metadata_consumers() {
        for artist in ["Unknown", "unknown", "UNKNOWN", " UnKnOwN\n"] {
            let metadata = MediaMetadata {
                title: Some("Movie".into()),
                artist: present_artist(Some(single_line_metadata(artist))),
            };
            assert!(metadata.artist().is_none());
            assert_eq!(
                metadata.overlay_text(None).to_str().unwrap(),
                "TITLE: MOVIE"
            );
        }
        assert_eq!(
            present_artist(Some("Unknown Mortal Orchestra".into())).as_deref(),
            Some("Unknown Mortal Orchestra")
        );
    }
}
