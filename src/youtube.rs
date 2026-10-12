//! Optional YouTube URL resolution through the user's installed yt-dlp.
use crate::Result;
use crate::metadata::single_line_metadata;
use crate::source::MediaSource;
use std::process::Command;

pub(crate) fn is_youtube_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some((host, port)) = authority.split_once(':') {
        if port.parse::<u16>().is_err() {
            return false;
        }
        host
    } else {
        authority
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["youtube.com", "youtu.be", "youtube-nocookie.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn http_url(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.split(['/', '?', '#']).next().unwrap_or("").is_empty()
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

// yt-dlp's JSON string conversion keeps newlines and control characters out of
// the line-based result. Only JSON strings are needed, not a general JSON parser.
fn metadata_string(value: &str) -> Option<String> {
    let mut chars = value.strip_prefix('"')?.strip_suffix('"')?.chars();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            result.push(match chars.next()? {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{08}',
                'f' => '\u{0c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'u' => {
                    let read_hex = |chars: &mut std::str::Chars<'_>| -> Option<u32> {
                        (0..4).try_fold(0, |code, _| Some(code * 16 + chars.next()?.to_digit(16)?))
                    };
                    let first = read_hex(&mut chars)?;
                    let code = if (0xd800..=0xdbff).contains(&first) {
                        if chars.next()? != '\\' || chars.next()? != 'u' {
                            return None;
                        }
                        let second = read_hex(&mut chars)?;
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return None;
                        }
                        0x10000 + ((first - 0xd800) << 10) + second - 0xdc00
                    } else {
                        first
                    };
                    char::from_u32(code)?
                }
                _ => return None,
            });
        } else if c == '"' || c.is_control() {
            return None;
        } else {
            result.push(c);
        }
    }
    let result = single_line_metadata(&result);
    (!result.is_empty()).then_some(result)
}

fn resolved_source(url: String, output: &str) -> Result<MediaSource> {
    let lines: Vec<_> = output.lines().collect();
    if lines.len() != 7 {
        return Err("yt-dlp did not return a usable YouTube stream URL".into());
    }
    // HLS video-only renditions use their master, which includes audio groups.
    // A direct URL is safe only for the combined audio/video fallback format.
    let stream = if http_url(lines[0]) {
        lines[0]
    } else if lines[2] != "none" && lines[2] != "NA" && !lines[2].is_empty() && http_url(lines[1]) {
        lines[1]
    } else {
        return Err("yt-dlp could not find a playable YouTube stream with audio".into());
    };
    Ok(MediaSource::YouTube {
        url,
        stream: stream.to_owned(),
        title: metadata_string(lines[3]).unwrap_or_else(|| "YouTube video".into()),
        channel: metadata_string(lines[4]),
        // Live streams use a moving timeline, so their positions are not stable.
        resume_id: (matches!(lines[6], "not_live" | "was_live")
            && lines[5].len() == 11
            && lines[5]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
        .then(|| lines[5].to_owned()),
    })
}

pub(crate) fn resolve(source: MediaSource) -> Result<MediaSource> {
    let MediaSource::Http(url) = &source else {
        return Ok(source);
    };
    if !is_youtube_url(url) {
        return Ok(source);
    }
    eprintln!("YouTube: resolving video with yt-dlp…");
    let output = Command::new("yt-dlp")
        .args([
            "--ignore-config",
            "--no-playlist",
            "--playlist-items",
            "1",
            "--simulate",
            "--no-progress",
            "--socket-timeout",
            "15",
            "--retries",
            "2",
            "--extractor-retries",
            "2",
            "-f",
            "bestvideo[protocol^=m3u8]/best[protocol^=m3u8]/best[protocol^=http]",
            "--print",
            "%(manifest_url)s",
            "--print",
            "%(url)s",
            "--print",
            "%(acodec)s",
            "--print",
            "%(title)j",
            "--print",
            "%(channel,uploader|)j",
            "--print",
            "%(id)s",
            "--print",
            "%(live_status)s",
            "--",
        ])
        .arg(url)
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "YouTube playback requires yt-dlp; install it and make sure it is on PATH"
                    .to_owned()
            } else {
                format!("could not start yt-dlp: {error}")
            }
        })?;
    if !output.status.success() {
        let details = String::from_utf8_lossy(&output.stderr);
        let message = details
            .lines()
            .rev()
            .find(|line| line.starts_with("ERROR:"))
            .or_else(|| details.lines().rev().find(|line| !line.trim().is_empty()));
        return Err(format!(
            "could not resolve YouTube video: {}",
            message
                .map(single_line_metadata)
                .unwrap_or_else(|| format!("yt-dlp exited with {}", output.status))
        ));
    }
    let output = std::str::from_utf8(&output.stdout)
        .map_err(|_| "yt-dlp returned invalid text instead of a stream URL".to_owned())?;
    resolved_source(url.clone(), output)
}

pub(crate) fn playback_error(error: &str) -> String {
    let network_error = [
        "could not open media",
        "could not inspect media",
        "could not read media packet",
        "could not restart selected HLS streams",
        "could not seek",
    ]
    .iter()
    .any(|message| error.contains(message));
    if network_error {
        format!(
            "YouTube playback stopped: {error}. The stream may have expired or become unavailable; reopen the YouTube URL to request a fresh stream."
        )
    } else {
        format!("YouTube playback stopped: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MediaInput;

    #[test]
    fn recognizes_youtube_hosts_without_matching_lookalikes() {
        for url in [
            "https://www.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "https://m.youtube.com/shorts/abc",
            "HTTPS://YOUTUBE.COM/live/abc",
            "https://www.youtube-nocookie.com/embed/abc",
        ] {
            assert!(is_youtube_url(url), "{url}");
        }
        for url in [
            "https://youtube.com.evil.example/watch",
            "https://notyoutube.com/watch",
            "https://youtube.com@evil.example/watch",
            "https://youtube.com:443@evil.example/watch",
            "file:///youtube.com",
            "movie.mkv",
        ] {
            assert!(!is_youtube_url(url), "{url}");
        }
    }

    #[test]
    fn resolves_master_and_keeps_original_url_and_metadata() {
        let original = "https://youtu.be/abc";
        let source = resolved_source(original.into(), "https://cdn.example/master.m3u8\nhttps://cdn.example/video.m3u8\nnone\n\"A title\\nwith \\u0101 and \\ud83d\\ude80\"\n\"Channel\"\ndQw4w9WgXcQ\nnot_live\n").unwrap();
        assert_eq!(source.input_bytes(), b"https://cdn.example/master.m3u8");
        assert_eq!(source.display(), original);
        assert!(source.is_network());
        assert!(source.local_path().is_none());
        assert_eq!(
            crate::metadata::source_title(&source),
            "A title with ā and 🚀"
        );
        let mut metadata = unsafe { crate::metadata::MediaMetadata::inspect(std::ptr::null()) };
        metadata.apply_source(&source);
        assert_eq!(metadata.title(), Some("A title with ā and 🚀"));
        assert_eq!(metadata.artist(), Some("Channel"));
        assert!(
            metadata
                .overlay_text(None)
                .to_string_lossy()
                .contains("CHANNEL")
        );
    }

    #[test]
    fn rejects_missing_invalid_and_silent_video_results() {
        for output in [
            "",
            "NA\n",
            "NA\nNA\nNA\nnull\nnull\ndQw4w9WgXcQ\nnot_live\n",
            "file:///tmp/video\nNA\nnone\n\"Title\"\n\"Channel\"\ndQw4w9WgXcQ\nnot_live\n",
            "NA\nhttps://cdn.example/video.mp4\nnone\n\"Title\"\n\"Channel\"\ndQw4w9WgXcQ\nnot_live\n",
        ] {
            assert!(resolved_source("https://youtu.be/abc".into(), output).is_err());
        }
        let combined = resolved_source(
            "https://youtu.be/abc".into(),
            "NA\nhttps://cdn.example/video.mp4\naac\nnull\nnull\ndQw4w9WgXcQ\nnot_live\n",
        )
        .unwrap();
        assert_eq!(combined.input_bytes(), b"https://cdn.example/video.mp4");
    }

    #[test]
    fn resume_identity_uses_extracted_id_and_excludes_live_streams() {
        for url in [
            "https://youtu.be/dQw4w9WgXcQ?si=tracking",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
        ] {
            for status in ["not_live", "was_live", "is_live", "post_live", "NA"] {
                let output = format!(
                    "https://cdn.example/master.m3u8?expire=123\nNA\nnone\nnull\nnull\ndQw4w9WgXcQ\n{status}\n"
                );
                let MediaSource::YouTube { resume_id, .. } =
                    resolved_source(url.into(), &output).unwrap()
                else {
                    panic!("expected YouTube source");
                };
                assert_eq!(
                    resume_id.as_deref(),
                    matches!(status, "not_live" | "was_live").then_some("dQw4w9WgXcQ")
                );
            }
        }
    }

    #[test]
    fn metadata_is_safe_for_c_strings_and_malformed_escapes() {
        assert_eq!(
            metadata_string(r#""one\u0000two\tthree""#).as_deref(),
            Some("onetwo three")
        );
        for bad in [
            r#""\ud800""#,
            r#""\udc00""#,
            r#""\uXYZA""#,
            r#""\q""#,
            "null",
        ] {
            assert!(metadata_string(bad).is_none());
        }
    }
}
