//! Human-editable key=value preferences. No config is written until a setting changes.
use crate::Result;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_CORNER_RADIUS: u16 = 32;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn parse_radius(value: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .ok()
        .filter(|radius| *radius <= MAX_CORNER_RADIUS)
        .ok_or_else(|| format!("corner_radius must be an integer from 0 to {MAX_CORNER_RADIUS}"))
}

fn config_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".config"))
        })
        .map(|p| p.join("undefined-player/config"))
}

fn read(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn setting(line: &str) -> Result<Option<(&str, &str)>> {
    let text = line.split('#').next().unwrap_or("").trim();
    if text.is_empty() {
        return Ok(None);
    }
    let (key, value) = text.split_once('=').ok_or("expected key = value")?;
    let (key, value) = (key.trim(), value.trim());
    if key.is_empty()
        || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        || value.is_empty()
    {
        return Err("expected a lowercase setting name and a nonempty value".into());
    }
    Ok(Some((key, value)))
}

struct Settings {
    corner_radius: u16,
    preferred_audio_language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            corner_radius: 0,
            preferred_audio_language: "en-US".into(),
        }
    }
}

fn parse(text: &str) -> Result<Settings> {
    let mut radius = None;
    let mut language = None;
    for (index, line) in text.lines().enumerate() {
        let result = (|| {
            if let Some((key, value)) = setting(line)? {
                match key {
                    "corner_radius" => {
                        if radius.is_some() {
                            return Err("duplicate corner_radius".into());
                        }
                        radius = Some(parse_radius(value)?);
                    }
                    "preferred_audio_language" => {
                        if language.is_some() {
                            return Err("duplicate preferred_audio_language".into());
                        }
                        language = Some(crate::language::parse_preference(value)?);
                    }
                    _ => return Err(format!("unknown setting: {key}")),
                }
            }
            Ok(())
        })();
        result.map_err(|error: String| format!("line {}: {error}", index + 1))?;
    }
    let defaults = Settings::default();
    Ok(Settings {
        corner_radius: radius.unwrap_or(defaults.corner_radius),
        preferred_audio_language: language.unwrap_or(defaults.preferred_audio_language),
    })
}

pub(crate) struct Config {
    path: Option<PathBuf>,
    pub(crate) corner_radius: u16,
    pub(crate) preferred_audio_language: String,
}

impl Config {
    pub(crate) fn load() -> Self {
        let path = config_path();
        let settings = path.as_deref().map_or_else(Settings::default, |path| {
            match read(path).and_then(|text| parse(&text)) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!(
                        "warning: invalid config {}: {error}; using defaults",
                        path.display()
                    );
                    Settings::default()
                }
            }
        });
        Self {
            path,
            corner_radius: settings.corner_radius,
            preferred_audio_language: settings.preferred_audio_language,
        }
    }

    pub(crate) fn set_corner_radius(&mut self, radius: u16) -> Result<()> {
        if radius > MAX_CORNER_RADIUS {
            return Err("corner radius is out of range".into());
        }
        self.save_setting(
            "corner_radius",
            &radius.to_string(),
            "# Window corner radius in logical pixels (0-32). 0 disables rounding.\n\
             # Fullscreen and maximized windows always have square corners.\n",
        )?;
        self.corner_radius = radius;
        Ok(())
    }

    pub(crate) fn set_preferred_audio_language(&mut self, value: &str) -> Result<()> {
        let language = crate::language::parse_preference(value)?;
        self.save_setting(
            "preferred_audio_language",
            &language,
            "# Preferred audio language, e.g. en_US, eng, pt-BR. Default: en_US.\n\
             # Use auto to keep the normal default track.\n",
        )?;
        self.preferred_audio_language = language;
        Ok(())
    }

    fn save_setting(&self, name: &str, value: &str, comment: &str) -> Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or("no XDG config directory is available")?;
        let directory = path.parent().ok_or("config path has no parent")?;
        let result = (|| -> Result<()> {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)
                .map_err(|e| e.to_string())?;
            let lock = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(directory.join("config.lock"))
                .map_err(|e| e.to_string())?;
            lock.try_lock()
                .map_err(|e| format!("could not lock config: {e}"))?;
            let old = read(path)?;
            // Never replace a malformed user file or apply only part of it.
            parse(&old)?;
            let mut found = false;
            let mut text = String::new();
            for line in old.lines() {
                if setting(line)?.is_some_and(|(key, _)| key == name) {
                    found = true;
                    text.push_str(&format!("{name} = {value}"));
                    if let Some((_, comment)) = line.split_once('#') {
                        text.push_str(" #");
                        text.push_str(comment);
                    }
                } else {
                    text.push_str(line);
                }
                text.push('\n');
            }
            if !found {
                text.push_str(comment);
                text.push_str(&format!("{name} = {value}\n"));
            }
            let temporary = directory.join(format!(
                ".config-{}-{}.tmp",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            let write_result = (|| -> io::Result<()> {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                fs::rename(&temporary, path)
            })();
            if write_result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            write_result.map_err(|e| e.to_string())
        })();
        result.map_err(|e| format!("could not save {}: {e}", path.display()))?;
        Ok(())
    }
}
