//! Select a master-playlist rendition before FFmpeg opens its media playlists.
//! Keep the selected rendition's track groups and all global playlist tags.
use std::ffi::CString;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn up_hls_master_playlists(
    buffer: *const u8,
    size: i32,
    opaque: *mut std::ffi::c_void,
    register: unsafe extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_char, i32),
) {
    let bytes = unsafe { std::slice::from_raw_parts(buffer, size as usize) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let mut variant = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("#EXT-X-STREAM-INF:") {
            variant = true;
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-MEDIA:") {
            if let Some(uri) = attribute(attrs, "URI").and_then(|uri| CString::new(uri).ok()) {
                unsafe {
                    register(
                        opaque,
                        uri.as_ptr(),
                        i32::from(attribute(attrs, "TYPE") == Some("SUBTITLES")),
                    )
                };
            }
        } else if variant && !line.is_empty() && !line.starts_with('#') {
            if let Ok(uri) = CString::new(line) {
                unsafe { register(opaque, uri.as_ptr(), 0) };
            }
            variant = false;
        }
    }
}

/// Register segment references from a subtitle playlist.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn up_hls_subtitle_segments(
    buffer: *const u8,
    size: i32,
    opaque: *mut std::ffi::c_void,
    register: unsafe extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_char),
) {
    let bytes = unsafe { std::slice::from_raw_parts(buffer, size as usize) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    for line in text.lines().map(str::trim) {
        if !line.is_empty()
            && !line.starts_with('#')
            && let Ok(uri) = CString::new(line)
        {
            unsafe { register(opaque, uri.as_ptr()) };
        }
    }
}

fn attribute<'a>(mut list: &'a str, key: &str) -> Option<&'a str> {
    while !list.is_empty() {
        let (name, rest) = list.split_once('=')?;
        let (value, rest) = if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"')?;
            (&quoted[..end], &quoted[end + 1..])
        } else {
            rest.split_once(',').unwrap_or((rest, ""))
        };
        if name.trim() == key {
            return Some(value.trim());
        }
        list = rest.trim_start_matches(',').trim_start();
    }
    None
}

fn supported_codecs(attributes: &str) -> bool {
    let Some(codecs) = attribute(attributes, "CODECS") else {
        return true;
    };
    codecs.split(',').all(|codec| {
        let name = match codec.trim().split('.').next().unwrap_or("") {
            "avc1" | "avc3" => "h264",
            "hvc1" | "hev1" => "hevc",
            "av01" => "av1",
            "vp09" => "vp9",
            "mp4v" => "mpeg4",
            // Unknown codec strings and audio codecs must not rule out video.
            _ => return true,
        };
        let name = CString::new(name).unwrap();
        unsafe { crate::ffi::up_av_video_decoder_available(name.as_ptr()) != 0 }
    })
}

fn select(master: &str) -> Option<String> {
    let lines: Vec<_> = master.lines().collect();
    let mut variants = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") else {
            continue;
        };
        let uri = (index + 1..lines.len())
            .find(|&i| !lines[i].trim().is_empty() && !lines[i].starts_with('#'))?;
        // Without resolution metadata, retain FFmpeg's full discovery path.
        let (width, height) = attribute(attributes, "RESOLUTION")?.split_once('x')?;
        let pixels = width.parse::<u32>().ok()? as u64 * height.parse::<u32>().ok()? as u64;
        if pixels == 0 {
            return None;
        }
        let fps = attribute(attributes, "FRAME-RATE")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(0.0);
        let bitrate = attribute(attributes, "BANDWIDTH")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        variants.push((index, uri, attributes, pixels, fps, bitrate));
    }
    if variants.len() < 2 {
        return None;
    }
    let selected = variants
        .iter()
        .filter(|v| supported_codecs(v.2))
        .max_by(|a, b| {
            a.3.cmp(&b.3)
                .then_with(|| a.4.total_cmp(&b.4))
                .then_with(|| a.5.cmp(&b.5))
                .then_with(|| b.0.cmp(&a.0))
        })?;
    let mut output = String::with_capacity(master.len());
    for (index, line) in lines.iter().enumerate() {
        if variants
            .iter()
            .any(|v| (index == v.0 || index == v.1) && v.0 != selected.0)
            || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
        {
            continue;
        }
        if let Some(media) = line.strip_prefix("#EXT-X-MEDIA:") {
            let kind = attribute(media, "TYPE")?;
            let group = attribute(media, "GROUP-ID")?;
            if attribute(selected.2, kind) != Some(group) {
                continue;
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    (output.len() < master.len()).then_some(output)
}

/// The native adapter owns the buffer; selection only removes playlist lines.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn up_hls_select_variant(buffer: *mut u8, size: i32) -> i32 {
    let bytes = unsafe { std::slice::from_raw_parts(buffer, size as usize) };
    let Some(filtered) = std::str::from_utf8(bytes).ok().and_then(select) else {
        return size;
    };
    unsafe { std::ptr::copy_nonoverlapping(filtered.as_ptr(), buffer, filtered.len()) };
    filtered.len() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};

    unsafe extern "C" fn collect_playlist(opaque: *mut c_void, uri: *const c_char, subtitle: i32) {
        let entries = unsafe { &mut *opaque.cast::<Vec<(String, bool)>>() };
        entries.push((
            unsafe { CStr::from_ptr(uri) }.to_str().unwrap().into(),
            subtitle != 0,
        ));
    }

    unsafe extern "C" fn collect_segment(opaque: *mut c_void, uri: *const c_char) {
        unsafe { collect_playlist(opaque, uri, 1) };
    }

    #[test]
    fn declared_subtitles_are_distinguished_from_video_and_audio() {
        let text = concat!(
            "#EXTM3U\r\n",
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",URI=\"audio\"\n",
            "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"s\",NAME=\"A, B\",URI=\"captions?x=1,2\"\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=100000\n# comment\n\nvideo\n",
        );
        let mut entries = Vec::<(String, bool)>::new();
        unsafe {
            up_hls_master_playlists(
                text.as_ptr(),
                text.len() as i32,
                (&mut entries as *mut Vec<(String, bool)>).cast(),
                collect_playlist,
            )
        };
        assert_eq!(
            entries,
            [
                ("audio".into(), false),
                ("captions?x=1,2".into(), true),
                ("video".into(), false)
            ]
        );
    }

    #[test]
    fn caption_segment_registration_keeps_signed_uris_intact() {
        let text = "#EXTM3U\n#EXTINF:3,\n../timedtext?fmt=vtt&signature=a%2Fb\r\n#EXT-X-ENDLIST\n";
        let mut entries = Vec::<(String, bool)>::new();
        unsafe {
            up_hls_subtitle_segments(
                text.as_ptr(),
                text.len() as i32,
                (&mut entries as *mut Vec<(String, bool)>).cast(),
                collect_segment,
            )
        };
        assert_eq!(
            entries,
            [("../timedtext?fmt=vtt&signature=a%2Fb".into(), true)]
        );
    }
}
